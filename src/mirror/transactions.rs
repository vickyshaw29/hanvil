//! `/api/v1/transactions` and `/api/v1/transactions/{transactionId}`.
//!
//! Hanvil executes Ethereum transactions through JSON-RPC, but the mirror has to show them as the
//! `ETHEREUMTRANSACTION` records they are on Hedera (`docs/code-plan.md` §5): a payer, a fee paid
//! to 0.0.98, a consensus timestamp that identifies the record, and a transaction id built from
//! the payer and that timestamp.

use std::collections::HashSet;

use axum::extract::{Path, State};
use axum::response::Json;
use serde_json::{Value, json};

use super::shapes::{self, timestamp};
use super::{Answer, Error, Order, Params, page, submitted};
use crate::evm::units::Tinybar;
use crate::serve::Shared;
use crate::state::{Chain, EntityId, NODE, Record, Timestamp, TxRecord};

/// Seconds a transaction stays valid for after its start. The SDK's default, and what the mirror
/// reports for a transaction that did not set one.
const VALID_DURATION_SECONDS: u64 = 120;

/// `GET /api/v1/transactions`.
pub async fn list(State(chain): State<Shared>, params: Params) -> Answer {
    params.only(&[
        "limit",
        "order",
        "account.id",
        "transactiontype",
        "result",
        "timestamp",
    ])?;
    let limit = params.limit()?;
    let order = params.order(Order::Desc)?;
    let account = params.entity_filter("account.id")?;
    let timestamps = params.timestamps()?;
    let kind = params.get("transactiontype").map(str::to_uppercase);
    let result = match params.get("result") {
        None => None,
        Some("success") => Some(true),
        Some("fail") => Some(false),
        Some(_) => return Err(Error::invalid_parameter("result")),
    };

    if kind
        .as_deref()
        .is_some_and(|kind| !TRANSACTION_TYPES.contains(&kind))
    {
        return Err(Error::invalid_parameter("transactiontype"));
    }

    let chain = chain.read();
    let matched = all_records(&chain)
        .into_iter()
        .filter(|entry| kind.as_deref().is_none_or(|kind| kind == entry.name))
        .filter(|entry| result.is_none_or(|want| want == entry.succeeded))
        .filter(|entry| account.is_none_or(|id| entry.involves(id)))
        .filter(|entry| timestamps.iter().all(|clause| clause.matches(entry.at)))
        .map(|entry| entry.body)
        .collect();
    Ok(Json(json!({
        "transactions": page(matched, order, limit),
        "links": shapes::links(),
    })))
}

/// One rendered transaction with the fields the list filters on, so an EVM record and a HAPI
/// record are filtered the same way.
struct Entry {
    at: Timestamp,
    name: &'static str,
    succeeded: bool,
    body: Value,
}

impl Entry {
    fn involves(&self, id: EntityId) -> bool {
        names_account(&self.body["transfers"], id)
    }
}

/// Whether a rendered transfer list names `id`.
fn names_account(transfers: &Value, id: EntityId) -> bool {
    transfers.as_array().is_some_and(|entries| {
        entries
            .iter()
            .any(|entry| entry["account"] == json!(id.to_string()))
    })
}

/// Every transaction the chain holds, oldest first: the EVM ones, and the HAPI ones. An EVM
/// transaction submitted through `callEthereum` has both a `TxRecord` and a HAPI record; the HAPI
/// record wins, because it carries the transaction id the client will ask for.
fn all_records(chain: &Chain) -> Vec<Entry> {
    let wrapped: HashSet<&[u8]> = chain
        .hapi_records()
        .map(|record| record.ethereum_hash.as_slice())
        .filter(|hash| !hash.is_empty())
        .collect();

    let mut entries: Vec<Entry> = chain
        .transactions()
        .filter(|tx| !wrapped.contains(tx.hash.as_slice()))
        .map(|tx| Entry {
            at: tx.consensus_timestamp,
            name: NAME,
            succeeded: tx.receipt.success,
            body: record(chain, tx),
        })
        .chain(chain.hapi_records().map(|found| Entry {
            at: found.consensus_timestamp,
            name: found.kind.name(),
            succeeded: found.status == crate::state::Status::Success,
            body: hapi_record(found),
        }))
        .collect();
    entries.sort_by_key(|entry| entry.at);
    entries
}

/// `openapi.yml:4236` Transaction, as a HAPI body fills it in.
fn hapi_record(found: &Record) -> Value {
    json!({
        "bytes": Value::Null,
        "charged_tx_fee": found.charged_fee.0,
        "consensus_timestamp": timestamp(found.consensus_timestamp).as_str(),
        "entity_id": found.entity().map_or(Value::Null, |id| json!(id.to_string())),
        "max_fee": found.max_fee.0.to_string(),
        "memo_base64": shapes::base64(found.memo.as_bytes()),
        "name": found.kind.name(),
        "nft_transfers": [],
        "node": NODE.to_string(),
        "nonce": found.id.nonce,
        "parent_consensus_timestamp": Value::Null,
        "result": found.status.name(),
        "scheduled": found.id.scheduled,
        "staking_reward_transfers": [],
        "token_transfers": [],
        "transaction_hash": shapes::base64(found.hash.as_bytes()),
        "transaction_id": found.id.to_string(),
        "transfers": Value::Array(
            found
                .transfers
                .iter()
                .map(|transfer| {
                    json!({
                        "account": transfer.account.to_string(),
                        "amount": transfer.amount,
                        "is_approval": false,
                    })
                })
                .collect(),
        ),
        "valid_duration_seconds": found.valid_duration_seconds.to_string(),
        "valid_start_timestamp": timestamp(found.id.valid_start).as_str(),
    })
}

/// `GET /api/v1/transactions/{transactionId}`. The id is `0.0.x-sss-nnn`; the SDK's `@` form is a
/// 400, which is what PR #39's reader stops polling on.
pub async fn by_id(State(chain): State<Shared>, Path(id): Path<String>, params: Params) -> Answer {
    params.only(&[])?;
    let (payer, valid_start) = shapes::parse_transaction_id(&id)?;
    let chain = chain.read();
    let mut found: Vec<Value> = chain
        .hapi_records()
        .filter(|record| record.id.payer == payer && record.id.valid_start == valid_start)
        .map(hapi_record)
        .collect();
    // An EVM transaction has no client-supplied valid start, so its id is built from the payer
    // and the consensus timestamp (`docs/code-plan.md` §5).
    found.extend(
        chain
            .transactions()
            .filter(|tx| {
                tx.consensus_timestamp == valid_start && payer_of(&chain, tx) == Some(payer)
            })
            .map(|tx| record(&chain, tx)),
    );
    if found.is_empty() {
        return Err(Error::NotFound {
            message: "Transaction not found".to_string(),
            detail: None,
        });
    }
    Ok(Json(json!({ "transactions": found })))
}

/// The type every EVM transaction is recorded under.
pub const NAME: &str = "ETHEREUMTRANSACTION";

/// Every value `?transactiontype=` accepts, copied from `openapi.yml:4063` TransactionTypes. A
/// type on this list that Hanvil never records answers with an empty page; a type that is not on
/// it is a caller's typo and answers 400.
const TRANSACTION_TYPES: [&str; 68] = [
    "ATOMICBATCH",
    "CONSENSUSCREATETOPIC",
    "CONSENSUSDELETETOPIC",
    "CONSENSUSSUBMITMESSAGE",
    "CONSENSUSUPDATETOPIC",
    "CONTRACTCALL",
    "CONTRACTCREATEINSTANCE",
    "CONTRACTDELETEINSTANCE",
    "CONTRACTUPDATEINSTANCE",
    "CRSPUBLICATION",
    "CRYPTOADDLIVEHASH",
    "CRYPTOAPPROVEALLOWANCE",
    "CRYPTOCREATEACCOUNT",
    "CRYPTODELETE",
    "CRYPTODELETEALLOWANCE",
    "CRYPTODELETELIVEHASH",
    "CRYPTOTRANSFER",
    "CRYPTOUPDATEACCOUNT",
    "ETHEREUMTRANSACTION",
    "FILEAPPEND",
    "FILECREATE",
    "FILEDELETE",
    "FILEUPDATE",
    "FREEZE",
    "HINTSKEYPUBLICATION",
    "HINTSPARTIALSIGNATURE",
    "HINTSPREPROCESSINGVOTE",
    "HISTORYPROOFKEYPUBLICATION",
    "HISTORYPROOFSIGNATURE",
    "HISTORYPROOFVOTE",
    "HOOKSTORE",
    "LEDGERIDPUBLICATION",
    "MIGRATIONROOTHASHVOTE",
    "NODECREATE",
    "NODEDELETE",
    "NODESTAKEUPDATE",
    "NODEUPDATE",
    "REGISTEREDNODECREATE",
    "REGISTEREDNODEDELETE",
    "REGISTEREDNODEUPDATE",
    "SCHEDULECREATE",
    "SCHEDULEDELETE",
    "SCHEDULESIGN",
    "STATESIGNATURETRANSACTION",
    "SYSTEMDELETE",
    "SYSTEMUNDELETE",
    "TOKENAIRDROP",
    "TOKENASSOCIATE",
    "TOKENBURN",
    "TOKENCANCELAIRDROP",
    "TOKENCLAIMAIRDROP",
    "TOKENCREATION",
    "TOKENDELETION",
    "TOKENDISSOCIATE",
    "TOKENFEESCHEDULEUPDATE",
    "TOKENFREEZE",
    "TOKENGRANTKYC",
    "TOKENMINT",
    "TOKENPAUSE",
    "TOKENREJECT",
    "TOKENREVOKEKYC",
    "TOKENUNFREEZE",
    "TOKENUNPAUSE",
    "TOKENUPDATE",
    "TOKENUPDATENFTS",
    "TOKENWIPE",
    "UNCHECKEDSUBMIT",
    "UTILPRNG",
];

/// `openapi.yml:4236` Transaction, as an EVM transaction fills it in.
pub(super) fn record(chain: &Chain, tx: &TxRecord) -> Value {
    let submitted = submitted::decode(tx);
    let fee = fee(tx);
    json!({
        "bytes": Value::Null,
        "charged_tx_fee": fee.0,
        "consensus_timestamp": timestamp(tx.consensus_timestamp).as_str(),
        "entity_id": entity_id(chain, tx).map_or(Value::Null, |id| json!(id.to_string())),
        "max_fee": (submitted.gas_limit.saturating_mul(submitted.gas_price.0)).to_string(),
        "memo_base64": Value::Null,
        "name": NAME,
        "nft_transfers": [],
        "node": NODE.to_string(),
        "nonce": 0,
        "parent_consensus_timestamp": Value::Null,
        "result": result(tx),
        "scheduled": false,
        "staking_reward_transfers": [],
        "token_transfers": [],
        // The mirror carries a 48-byte record hash here; Hanvil has the 32-byte EVM hash and
        // says so rather than padding it into something that looks like one.
        "transaction_hash": shapes::base64(tx.hash.as_slice()),
        "transaction_id": transaction_id(chain, tx),
        "transfers": transfers(chain, tx),
        "valid_duration_seconds": VALID_DURATION_SECONDS.to_string(),
        // Hanvil reaches consensus at submission, so a transaction's start is its consensus time.
        "valid_start_timestamp": timestamp(tx.consensus_timestamp).as_str(),
    })
}

/// `SUCCESS`, or the `ResponseCodeEnum` name a reverting contract call gets on Hedera.
fn result(tx: &TxRecord) -> &'static str {
    if tx.receipt.success {
        "SUCCESS"
    } else {
        "CONTRACT_REVERT_EXECUTED"
    }
}

/// What the payer was charged, in tinybar.
fn fee(tx: &TxRecord) -> Tinybar {
    Tinybar(
        tx.receipt
            .gas_used
            .saturating_mul(tx.receipt.effective_gas_price),
    )
}

/// The entity the transaction acted on: the contract it created, else the one it called.
fn entity_id(chain: &Chain, tx: &TxRecord) -> Option<EntityId> {
    if let Some(created) = tx.receipt.contract_address {
        return chain.contract_id_by_evm(&created);
    }
    chain.entity_by_evm(&submitted::decode(tx).to?)
}

/// The payer's entity id. Every sender that can pay for gas has an account.
pub(super) fn payer_of(chain: &Chain, tx: &TxRecord) -> Option<EntityId> {
    chain.entity_by_evm(&tx.from)
}

/// `0.0.x-sss-nnn`, built from the payer and the consensus timestamp.
pub(super) fn transaction_id(chain: &Chain, tx: &TxRecord) -> String {
    let payer = payer_of(chain, tx).unwrap_or(EntityId(0));
    shapes::transaction_id(payer, tx.consensus_timestamp)
}

/// The HBAR that moved: the fee off the payer to 0.0.98, and the value the top-level call carried.
/// Value moved by an inner call is not itemised — the EVM does not report it and Hanvil does not
/// invent it (README, "what is not emulated").
fn transfers(chain: &Chain, tx: &TxRecord) -> Value {
    use crate::state::FEE_COLLECTOR;

    let submitted = submitted::decode(tx);
    let fee = fee(tx).0 as i128;
    let value = if tx.receipt.success {
        submitted.value.0 as i128
    } else {
        0
    };
    let payer = payer_of(chain, tx);
    let recipient = submitted
        .to
        .filter(|_| value > 0)
        .and_then(|to| chain.entity_by_evm(&to));

    let mut entries: Vec<(EntityId, i128)> = Vec::with_capacity(3);
    if let Some(payer) = payer {
        entries.push((payer, -(fee + value)));
    }
    if fee > 0 {
        entries.push((FEE_COLLECTOR, fee));
    }
    if let Some(recipient) = recipient {
        entries.push((recipient, value));
    }
    entries.sort_by_key(|(id, _)| *id);
    Value::Array(
        entries
            .into_iter()
            .map(|(account, amount)| {
                json!({ "account": account.to_string(), "amount": amount, "is_approval": false })
            })
            .collect(),
    )
}

/// Whether `id` is a party to the transaction, which is what `?account.id=` filters on.
pub(super) fn involves(chain: &Chain, tx: &TxRecord, id: EntityId) -> bool {
    names_account(&transfers(chain, tx), id)
}
