//! `/api/v1/contracts/...` — the contract-result endpoints a Hedera app reads instead of
//! `eth_getTransactionReceipt`, and the log endpoint the validator agent reads for events.

use alloy_primitives::{Address, B256};
use axum::extract::{Path, State};
use axum::response::Json;
use serde_json::{Value, json};

use super::shapes::{self, Reference, timestamp, timestamp_range};
use super::{Answer, Error, Order, Params, page, submitted, transactions};
use crate::serve::Shared;
use crate::state::{AUTO_RENEW_PERIOD_SECS, Chain, EntityId, StoredLog, TxRecord};

/// `GET /api/v1/contracts/{contractIdOrAddress}` — `openapi.yml:1586` ContractResponse.
pub async fn get(State(chain): State<Shared>, Path(id): Path<String>) -> Answer {
    let reference = shapes::parse_reference(&id, "contractIdOrAddress")?;
    let chain = chain.read();
    let (entity, address) = resolve(&chain, &reference).ok_or_else(Error::not_found)?;
    let created = chain
        .contract(entity)
        .and_then(|c| chain.block_by_number(c.created_block))
        .map_or(chain.genesis_timestamp(), |b| b.consensus_timestamp);
    Ok(Json(json!({
        "admin_key": Value::Null,
        "auto_renew_account": Value::Null,
        "auto_renew_period": AUTO_RENEW_PERIOD_SECS,
        "contract_id": entity.to_string(),
        "created_timestamp": timestamp(created).as_str(),
        "deleted": false,
        "evm_address": shapes::hex(address.as_slice()),
        "expiration_timestamp": Value::Null,
        // Hanvil deploys through the EVM, never through a file, so there is no file id to give.
        "file_id": Value::Null,
        "max_automatic_token_associations": 0,
        "memo": "",
        "nonce": chain.nonce_by_evm(&address),
        "obtainer_id": Value::Null,
        "permanent_removal": Value::Null,
        "proxy_account_id": Value::Null,
        "timestamp": timestamp_range(created, None),
        // The init code is not kept; the deployed code is what callers verify against.
        "bytecode": Value::Null,
        "runtime_bytecode": shapes::hex(&chain.code_by_evm(&address)),
    })))
}

/// `GET /api/v1/contracts/{contractIdOrAddress}/results`.
pub async fn results(
    State(chain): State<Shared>,
    Path(id): Path<String>,
    params: Params,
) -> Answer {
    let limit = params.limit()?;
    let order = params.order(Order::Desc)?;
    let reference = shapes::parse_reference(&id, "contractIdOrAddress")?;
    let chain = chain.read();
    let (_, address) = resolve(&chain, &reference).ok_or_else(Error::not_found)?;
    let matched: Vec<Value> = chain
        .transactions()
        .filter(|tx| touches(tx, address))
        .map(|tx| result_body(&chain, tx, false))
        .collect();
    Ok(Json(json!({
        "results": page(matched, order, limit),
        "links": shapes::links(),
    })))
}

/// `GET /api/v1/contracts/results/{transactionIdOrHash}` — `openapi.yml:2516`
/// ContractResultDetails. Takes the 32-byte EVM hash or a `0.0.x-sss-nnn` transaction id.
pub async fn result(State(chain): State<Shared>, Path(id): Path<String>) -> Answer {
    let chain = chain.read();
    let found = match id.strip_prefix("0x") {
        Some(body) => hex::decode(body)
            .ok()
            .filter(|bytes| bytes.len() == 32)
            .map(|bytes| B256::from_slice(&bytes))
            .ok_or_else(|| Error::invalid_parameter("transactionIdOrHash"))
            .map(|hash| chain.transaction(&hash))?,
        None => {
            let (payer, valid_start) = shapes::parse_transaction_id(&id)?;
            chain.transactions().find(|tx| {
                tx.consensus_timestamp == valid_start
                    && transactions::payer_of(&chain, tx) == Some(payer)
            })
        }
    };
    let tx = found.ok_or_else(Error::not_found)?;
    Ok(Json(result_body(&chain, tx, true)))
}

/// `GET /api/v1/contracts/results/logs` and `/contracts/{id}/results/logs`.
pub async fn logs(
    State(chain): State<Shared>,
    contract: Option<Path<String>>,
    params: Params,
) -> Answer {
    let limit = params.limit()?;
    let order = params.order(Order::Desc)?;
    let chain = chain.read();
    let only = match contract {
        None => None,
        Some(Path(id)) => {
            let reference = shapes::parse_reference(&id, "contractIdOrAddress")?;
            Some(resolve(&chain, &reference).ok_or_else(Error::not_found)?.1)
        }
    };
    let matched: Vec<Value> = chain
        .transactions()
        .flat_map(|tx| tx.receipt.logs.iter().map(move |log| (tx, log)))
        .filter(|(_, log)| only.is_none_or(|address| log.address == address))
        .map(|(tx, log)| log_body(&chain, tx, log, true))
        .collect();
    Ok(Json(json!({
        "logs": page(matched, order, limit),
        "links": shapes::links(),
    })))
}

/// An entity id and EVM address for a contract reference. Accounts are not contracts and 404.
fn resolve(chain: &Chain, reference: &Reference) -> Option<(EntityId, Address)> {
    match reference {
        Reference::Entity(id) => chain.contract(*id).map(|c| (*id, c.address)),
        Reference::Evm(address) => chain.contract_id_by_evm(address).map(|id| (id, *address)),
    }
}

/// Whether the transaction called or created this contract.
fn touches(tx: &TxRecord, address: Address) -> bool {
    tx.receipt.contract_address == Some(address) || submitted::decode(tx).to == Some(address)
}

/// `openapi.yml:2367` ContractResult; `details` adds the fields ContractResultDetails carries.
fn result_body(chain: &Chain, tx: &TxRecord, details: bool) -> Value {
    let submitted = submitted::decode(tx);
    let contract = tx.receipt.contract_address.or(submitted.to);
    let block = chain.block_by_number(tx.block_number);
    let mut body = json!({
        // Hanvil executes legacy and 1559 envelopes only, so neither list ever has an entry.
        "access_list": [],
        "authorization_list": [],
        "address": contract.map_or(Value::Null, |a| json!(shapes::hex(a.as_slice()))),
        "amount": submitted.value.0,
        "block_gas_used": block.map_or(0, |b| b.gas_used),
        "block_hash": shapes::hex(tx.block_hash.as_slice()),
        "block_number": tx.block_number,
        "bloom": shapes::hex(tx.receipt.logs_bloom.as_slice()),
        "call_result": shapes::hex(&tx.receipt.output),
        "chain_id": submitted
            .chain_id
            .map_or(Value::Null, |id| json!(format!("0x{id:x}"))),
        "contract_id": contract
            .and_then(|a| chain.contract_id_by_evm(&a))
            .map_or(Value::Null, |id| json!(id.to_string())),
        "created_contract_ids": tx
            .receipt
            .contract_address
            .and_then(|a| chain.contract_id_by_evm(&a))
            .map_or_else(Vec::new, |id| vec![id.to_string()]),
        "error_message": error_message(tx),
        "failed_initcode": Value::Null,
        "from": shapes::hex(tx.from.as_slice()),
        "function_parameters": shapes::hex(&submitted.input),
        "gas_consumed": tx.receipt.gas_used,
        "gas_limit": submitted.gas_limit,
        "gas_price": format!("0x{:x}", tx.receipt.effective_gas_price),
        "gas_used": tx.receipt.gas_used,
        "hash": shapes::hex(tx.hash.as_slice()),
        "max_fee_per_gas": format!("0x{:x}", submitted.gas_price.0),
        "max_priority_fee_per_gas": format!(
            "0x{:x}",
            submitted.priority_fee.unwrap_or_default().0
        ),
        "nonce": submitted.nonce,
        "r": submitted.signature.map_or(Value::Null, |s| json!(format!("0x{:x}", s.r()))),
        "result": if tx.receipt.success { "SUCCESS" } else { "CONTRACT_REVERT_EXECUTED" },
        "s": submitted.signature.map_or(Value::Null, |s| json!(format!("0x{:x}", s.s()))),
        "status": if tx.receipt.success { "0x1" } else { "0x0" },
        "timestamp": timestamp(tx.consensus_timestamp).as_str(),
        "to": submitted.to.map_or(Value::Null, |a| json!(shapes::hex(a.as_slice()))),
        "transaction_index": tx.index,
        "type": submitted.tx_type,
        "v": submitted.signature.map_or(Value::Null, |s| json!(u8::from(s.v()))),
    });
    if details {
        body["logs"] = Value::Array(
            tx.receipt
                .logs
                .iter()
                .map(|log| log_body(chain, tx, log, false))
                .collect(),
        );
        body["state_changes"] = json!([]);
    }
    body
}

/// The message the mirror shows for a failed execution: the revert data, or the halt reason.
fn error_message(tx: &TxRecord) -> Value {
    if tx.receipt.success {
        return Value::Null;
    }
    match &tx.receipt.halt_reason {
        Some(reason) => json!(reason),
        None => json!(shapes::hex(&tx.receipt.output)),
    }
}

/// `openapi.yml:2603` ContractResultLog; `standalone` adds what `openapi.yml:2257` ContractLog
/// carries when the log is not nested inside a result.
fn log_body(chain: &Chain, tx: &TxRecord, log: &StoredLog, standalone: bool) -> Value {
    let mut body = json!({
        "address": shapes::hex(log.address.as_slice()),
        "bloom": shapes::hex(tx.receipt.logs_bloom.as_slice()),
        "contract_id": chain
            .contract_id_by_evm(&log.address)
            .map_or(Value::Null, |id| json!(id.to_string())),
        "data": shapes::hex(&log.data),
        "index": log.log_index,
        "topics": log
            .topics
            .iter()
            .map(|t| shapes::hex(t.as_slice()))
            .collect::<Vec<_>>(),
    });
    if standalone {
        body["block_hash"] = json!(shapes::hex(log.block_hash.as_slice()));
        body["block_number"] = json!(log.block_number);
        body["root_contract_id"] = body["contract_id"].clone();
        body["timestamp"] = timestamp(tx.consensus_timestamp);
        body["transaction_hash"] = json!(shapes::hex(tx.hash.as_slice()));
        body["transaction_index"] = json!(tx.index);
    }
    body
}
