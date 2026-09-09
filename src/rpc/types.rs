//! Wire encodings for JSON-RPC: hex quantities, parameter parsing, and the block, transaction,
//! receipt and log objects in the shape hiero-json-rpc-relay returns them (`docs/openrpc.json`).

use alloy_consensus::{Transaction as _, TxEnvelope};
use alloy_eips::eip2718::Decodable2718 as _;
use alloy_primitives::{Address, B256, Bloom, Bytes, U256};
use serde_json::{Map, Value, json};

use super::RpcError;
use crate::evm::units::{Tinybar, WEIBAR_PER_TINYBAR};
use crate::state::{Block, CallRequest, Chain, LogFilter, StoredLog, TxBody, TxRecord, UnsignedTx};

/// `0x`-prefixed minimal hex, as every Ethereum QUANTITY is encoded. Zero is `0x0`.
pub fn quantity(value: U256) -> String {
    format!("0x{value:x}")
}

/// [`quantity`] for a `u64` (chain id, block number, nonce, gas).
pub fn quantity_u64(value: u64) -> String {
    quantity(U256::from(value))
}

/// A tinybar amount as the client sees it: weibar.
pub fn weibar(amount: Tinybar) -> String {
    quantity(amount.to_weibar())
}

/// A tinybar-per-gas price as the client sees it.
pub fn weibar_u64(tinybar: u64) -> String {
    quantity(U256::from(tinybar) * U256::from(WEIBAR_PER_TINYBAR))
}

/// `0x`-prefixed hex of arbitrary bytes; `0x` when empty.
pub fn data(bytes: &[u8]) -> String {
    format!("0x{}", hex::encode(bytes))
}

/// 32-byte hash, lowercase.
pub fn hash(value: &B256) -> String {
    format!("{value:#x}")
}

/// Address, lowercase, as the relay renders it.
pub fn address(value: &Address) -> String {
    format!("{value:#x}")
}

// ---- parameter parsing --------------------------------------------------------------------

/// A 20-byte address parameter.
pub fn parse_address(param: Option<&Value>) -> Result<Address, RpcError> {
    let text = param
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params("address must be a hex string"))?;
    text.parse::<Address>()
        .map_err(|_| RpcError::invalid_params(format!("`{text}` is not a 20-byte hex address")))
}

/// A 32-byte hash parameter.
pub fn parse_hash(param: Option<&Value>) -> Result<B256, RpcError> {
    let text = param
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params("hash must be a hex string"))?;
    text.parse::<B256>()
        .map_err(|_| RpcError::invalid_params(format!("`{text}` is not a 32-byte hex hash")))
}

/// A QUANTITY parameter.
pub fn parse_quantity(param: Option<&Value>, name: &str) -> Result<U256, RpcError> {
    match param {
        Some(Value::String(text)) => {
            let digits = text.strip_prefix("0x").unwrap_or(text);
            U256::from_str_radix(digits, 16)
                .map_err(|_| RpcError::invalid_params(format!("{name}: `{text}` is not hex")))
        }
        Some(Value::Number(n)) => n.as_u64().map(U256::from).ok_or_else(|| {
            RpcError::invalid_params(format!("{name} must be a non-negative integer"))
        }),
        _ => Err(RpcError::invalid_params(format!("{name} is required"))),
    }
}

/// A QUANTITY that must fit in `u64`.
pub fn parse_u64(param: Option<&Value>, name: &str) -> Result<u64, RpcError> {
    let value = parse_quantity(param, name)?;
    u64::try_from(value).map_err(|_| RpcError::invalid_params(format!("{name} exceeds 64 bits")))
}

/// Hex-encoded DATA parameter.
pub fn parse_bytes(param: Option<&Value>, name: &str) -> Result<Bytes, RpcError> {
    let text = param
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params(format!("{name} must be a hex string")))?;
    let digits = text.strip_prefix("0x").unwrap_or(text);
    hex::decode(digits)
        .map(Bytes::from)
        .map_err(|_| RpcError::invalid_params(format!("{name}: `{text}` is not hex")))
}

/// Block tag or number, resolved against the chain. `None` when the tag is absent (= latest).
/// Pending, safe and finalized all mean the head: Hanvil has no mempool and instant finality.
pub fn parse_block_number(chain: &Chain, param: Option<&Value>) -> Result<u64, RpcError> {
    let head = chain.block_number();
    match param {
        None | Some(Value::Null) => Ok(head),
        Some(Value::String(tag)) => match tag.as_str() {
            "latest" | "pending" | "safe" | "finalized" => Ok(head),
            "earliest" => Ok(0),
            _ => parse_u64(param, "block number"),
        },
        Some(Value::Object(spec)) => {
            if let Some(number) = spec.get("blockNumber") {
                return parse_block_number(chain, Some(number));
            }
            if let Some(hash) = spec.get("blockHash") {
                let hash = parse_hash(Some(hash))?;
                return chain
                    .block_by_hash(&hash)
                    .map(|b| b.number)
                    .ok_or_else(|| RpcError::server(format!("block {hash:#x} not found")));
            }
            Err(RpcError::invalid_params(
                "block spec needs blockNumber or blockHash",
            ))
        }
        Some(other) => Err(RpcError::invalid_params(format!(
            "block tag `{other}` is not a tag or a hex number"
        ))),
    }
}

/// The transaction object of `eth_call`, `eth_estimateGas` and `eth_sendTransaction`,
/// converted to tinybar. `input` wins over `data` when both are present, as in geth.
pub fn parse_call(param: Option<&Value>) -> Result<CallRequest, RpcError> {
    let object = param
        .and_then(Value::as_object)
        .ok_or_else(|| RpcError::invalid_params("first parameter must be a transaction object"))?;
    let optional_address = |key: &str| -> Result<Option<Address>, RpcError> {
        match object.get(key) {
            None | Some(Value::Null) => Ok(None),
            some => parse_address(some).map(Some),
        }
    };
    let optional_u64 = |key: &str| -> Result<Option<u64>, RpcError> {
        match object.get(key) {
            None | Some(Value::Null) => Ok(None),
            some => parse_u64(some, key).map(Some),
        }
    };
    let value = match object.get("value") {
        None | Some(Value::Null) => Tinybar(0),
        some => Tinybar::from_weibar_exact(parse_quantity(some, "value")?)
            .map_err(|e| RpcError::invalid_params(e.to_string()))?,
    };
    let gas_price = match object
        .get("gasPrice")
        .or_else(|| object.get("maxFeePerGas"))
    {
        None | Some(Value::Null) => None,
        some => Some(
            Tinybar::from_weibar_floor(parse_quantity(some, "gasPrice")?)
                .map_err(|e| RpcError::invalid_params(e.to_string()))?
                .0,
        ),
    };
    let input = match object.get("input").or_else(|| object.get("data")) {
        None | Some(Value::Null) => Bytes::new(),
        some => parse_bytes(some, "data")?,
    };
    Ok(CallRequest {
        from: optional_address("from")?,
        to: optional_address("to")?,
        gas: optional_u64("gas")?,
        gas_price,
        value: value.0,
        input,
    })
}

/// Optional `nonce` on an `eth_sendTransaction` object.
pub fn parse_optional_nonce(param: Option<&Value>) -> Result<Option<u64>, RpcError> {
    match param
        .and_then(Value::as_object)
        .and_then(|o| o.get("nonce"))
    {
        None | Some(Value::Null) => Ok(None),
        some => parse_u64(some, "nonce").map(Some),
    }
}

/// Whether a filter object pins its `toBlock` to a fixed block.
///
/// `parse_log_filter` resolves an absent or `latest` `toBlock` to the head, which is what
/// `eth_getLogs` wants and the opposite of what a standing `eth_newFilter` wants: a filter pinned
/// to the head at install time would never report a log again. A filter that names a number or
/// `earliest` means what it says and stops there.
pub fn to_block_is_pinned(param: Option<&Value>) -> bool {
    match param
        .and_then(Value::as_object)
        .and_then(|o| o.get("toBlock"))
    {
        None | Some(Value::Null) => false,
        Some(Value::String(tag)) => {
            !matches!(tag.as_str(), "latest" | "pending" | "safe" | "finalized")
        }
        Some(_) => true,
    }
}

/// `eth_getLogs` filter object.
pub fn parse_log_filter(chain: &Chain, param: Option<&Value>) -> Result<LogFilter, RpcError> {
    let object = param
        .and_then(Value::as_object)
        .ok_or_else(|| RpcError::invalid_params("first parameter must be a filter object"))?;
    let (from_block, to_block) = match object.get("blockHash") {
        Some(Value::String(_)) => {
            let hash = parse_hash(object.get("blockHash"))?;
            let number = chain
                .block_by_hash(&hash)
                .map(|b| b.number)
                .ok_or_else(|| RpcError::server(format!("block {hash:#x} not found")))?;
            (number, number)
        }
        _ => (
            parse_block_number(chain, object.get("fromBlock"))?,
            parse_block_number(chain, object.get("toBlock"))?,
        ),
    };
    let addresses = match object.get("address") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(list)) => list
            .iter()
            .map(|a| parse_address(Some(a)))
            .collect::<Result<_, _>>()?,
        some => vec![parse_address(some)?],
    };
    let topics = match object.get("topics") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(positions)) => positions
            .iter()
            .map(|clause| match clause {
                Value::Null => Ok(None),
                Value::Array(any_of) => any_of
                    .iter()
                    .map(|t| parse_hash(Some(t)))
                    .collect::<Result<Vec<_>, _>>()
                    .map(Some),
                single => parse_hash(Some(single)).map(|h| Some(vec![h])),
            })
            .collect::<Result<_, _>>()?,
        Some(_) => return Err(RpcError::invalid_params("topics must be an array")),
    };
    Ok(LogFilter {
        from_block,
        to_block,
        addresses,
        topics,
    })
}

// ---- rendering --------------------------------------------------------------------------------

/// Empty ommers list hash; constant on every post-merge chain.
const EMPTY_UNCLES: &str = "0x1dcc4de8dec75d7aab85b567b6ccd41ad312451b948a7413f0a142fd40d49347";
/// 32 zero bytes, which the relay uses for roots it has no value for.
const ZERO_32: &str = "0x0000000000000000000000000000000000000000000000000000000000000000";

/// Block object. `full` inlines transaction objects instead of hashes.
pub fn block_json(chain: &Chain, block: &Block, full: bool) -> Value {
    let transactions: Vec<Value> = block
        .transactions
        .iter()
        .map(|h| {
            if full {
                chain.transaction(h).map(tx_json).unwrap_or(Value::Null)
            } else {
                json!(hash(h))
            }
        })
        .collect();
    // Every field `Block` requires in the relay's openrpc.json is present. VERIFY the roots: the
    // schema requires stateRoot, transactionsRoot and receiptsRoot but not what a chain with no
    // Merkle tries puts in them, so these are zero and, for transactionsRoot, the block hash.
    json!({
        "number": quantity_u64(block.number),
        "hash": hash(&block.hash),
        "parentHash": hash(&block.parent_hash),
        "sha3Uncles": EMPTY_UNCLES,
        "miner": address(&Address::ZERO),
        "stateRoot": ZERO_32,
        "transactionsRoot": if block.transactions.is_empty() { ZERO_32.to_string() } else { hash(&block.hash) },
        "receiptsRoot": ZERO_32,
        "logsBloom": bloom(&block.logs_bloom),
        "difficulty": "0x0",
        "totalDifficulty": "0x0",
        "gasLimit": quantity_u64(block.gas_limit),
        "gasUsed": quantity_u64(block.gas_used),
        "timestamp": quantity_u64(block.timestamp),
        "extraData": "0x",
        "mixHash": ZERO_32,
        "nonce": "0x0000000000000000",
        "baseFeePerGas": weibar_u64(block.base_fee),
        "size": quantity_u64(block_size(chain, block)),
        "transactions": transactions,
        "uncles": [],
        "withdrawals": [],
        "withdrawalsRoot": ZERO_32,
    })
}

/// Header bytes plus the transactions' encoded length. Not an RLP size; the relay's `size` is
/// the mirror node's record-file size, which has no equivalent here. VERIFY against a real block.
fn block_size(chain: &Chain, block: &Block) -> u64 {
    let header = 500u64;
    let bodies: u64 = block
        .transactions
        .iter()
        .filter_map(|h| chain.transaction(h))
        .map(|tx| match &tx.body {
            TxBody::Signed(raw) => raw.len() as u64,
            TxBody::Unsigned(u) => 100 + u.input.len() as u64,
        })
        .sum();
    header + bodies
}

fn bloom(value: &Bloom) -> String {
    format!("{value:#x}")
}

/// `None` only for bytes that no longer decode, which the chain never holds: it decoded them once
/// to execute them. The renderers drop the envelope's fields rather than fail, the same stance
/// `mirror::submitted::decode` takes on the same impossible input.
fn decode_envelope(raw: &[u8]) -> Option<TxEnvelope> {
    TxEnvelope::decode_2718(&mut &raw[..]).ok()
}

/// Transaction object for `eth_getTransactionByHash` and full blocks.
pub fn tx_json(tx: &TxRecord) -> Value {
    let mut object = Map::new();
    object.insert("hash".into(), json!(hash(&tx.hash)));
    object.insert("from".into(), json!(address(&tx.from)));
    object.insert("blockHash".into(), json!(hash(&tx.block_hash)));
    object.insert("blockNumber".into(), json!(quantity_u64(tx.block_number)));
    object.insert("transactionIndex".into(), json!(quantity_u64(tx.index)));
    match &tx.body {
        TxBody::Signed(raw) => {
            if let Some(envelope) = decode_envelope(raw) {
                signed_fields(&mut object, &envelope);
            }
        }
        TxBody::Unsigned(unsigned) => unsigned_fields(&mut object, unsigned),
    }
    Value::Object(object)
}

/// An `eth_sendTransaction` body has no signature and no chain id, so those fields are zero and
/// null rather than absent: the relay returns the whole object either way.
fn unsigned_fields(object: &mut Map<String, Value>, unsigned: &UnsignedTx) {
    object.insert("type".into(), json!("0x0"));
    object.insert("nonce".into(), json!(quantity_u64(unsigned.nonce)));
    object.insert("gas".into(), json!(quantity_u64(unsigned.gas_limit)));
    object.insert("gasPrice".into(), json!(weibar_u64(unsigned.gas_price)));
    object.insert(
        "to".into(),
        unsigned
            .to
            .as_ref()
            .map(address)
            .map_or(Value::Null, Value::String),
    );
    object.insert("value".into(), json!(weibar_u64(unsigned.value)));
    object.insert("input".into(), json!(data(&unsigned.input)));
    object.insert("chainId".into(), Value::Null);
    object.insert("v".into(), json!("0x0"));
    object.insert("r".into(), json!("0x0"));
    object.insert("s".into(), json!("0x0"));
}

fn signed_fields(object: &mut Map<String, Value>, envelope: &TxEnvelope) {
    let signature = envelope.signature();
    object.insert(
        "type".into(),
        json!(quantity_u64(u64::from(envelope.tx_type() as u8))),
    );
    object.insert("nonce".into(), json!(quantity_u64(envelope.nonce())));
    object.insert("gas".into(), json!(quantity_u64(envelope.gas_limit())));
    object.insert(
        "gasPrice".into(),
        json!(quantity(U256::from(envelope.max_fee_per_gas()))),
    );
    object.insert(
        "to".into(),
        envelope
            .to()
            .as_ref()
            .map(address)
            .map_or(Value::Null, Value::String),
    );
    object.insert("value".into(), json!(quantity(envelope.value())));
    object.insert("input".into(), json!(data(envelope.input())));
    object.insert(
        "chainId".into(),
        envelope
            .chain_id()
            .map_or(Value::Null, |c| json!(quantity_u64(c))),
    );
    object.insert("r".into(), json!(quantity(signature.r())));
    object.insert("s".into(), json!(quantity(signature.s())));
    let parity = u64::from(signature.v());
    if envelope.is_legacy() {
        // EIP-155: v = chainId * 2 + 35 + parity; 27 + parity without a chain id.
        let v = match envelope.chain_id() {
            Some(chain_id) => chain_id * 2 + 35 + parity,
            None => 27 + parity,
        };
        object.insert("v".into(), json!(quantity_u64(v)));
    } else {
        object.insert("v".into(), json!(quantity_u64(parity)));
        object.insert("yParity".into(), json!(quantity_u64(parity)));
        object.insert(
            "maxFeePerGas".into(),
            json!(quantity(U256::from(envelope.max_fee_per_gas()))),
        );
        object.insert(
            "maxPriorityFeePerGas".into(),
            json!(quantity(U256::from(
                envelope.max_priority_fee_per_gas().unwrap_or(0)
            ))),
        );
        let access_list: Vec<Value> = envelope
            .access_list()
            .map(|list| {
                list.iter()
                    .map(|item| {
                        json!({
                            "address": address(&item.address),
                            "storageKeys": item.storage_keys.iter().map(hash).collect::<Vec<_>>(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        object.insert("accessList".into(), Value::Array(access_list));
    }
}

/// Receipt object (`ReceiptInfo` in the relay's openrpc.json): that schema's fields and no
/// others. A failed transaction carries no revert data here, because neither the relay nor Anvil
/// puts any on a receipt — `eth_call` returns it as `{code: 3, data}`, and the mirror's
/// `/contracts/results/{hash}` carries it as `error_message`.
pub fn receipt_json(tx: &TxRecord) -> Value {
    let (tx_type, to) = match &tx.body {
        TxBody::Signed(raw) => {
            decode_envelope(raw).map_or((0, None), |e| (e.tx_type() as u8, e.to()))
        }
        TxBody::Unsigned(unsigned) => (0, unsigned.to),
    };
    json!({
        "type": quantity_u64(u64::from(tx_type)),
        "transactionHash": hash(&tx.hash),
        "transactionIndex": quantity_u64(tx.index),
        "blockHash": hash(&tx.block_hash),
        "blockNumber": quantity_u64(tx.block_number),
        "from": address(&tx.from),
        "to": to.as_ref().map(address).map_or(Value::Null, Value::String),
        "cumulativeGasUsed": quantity_u64(tx.receipt.gas_used),
        "gasUsed": quantity_u64(tx.receipt.gas_used),
        "contractAddress": tx.receipt.contract_address.as_ref().map(address).map_or(Value::Null, Value::String),
        "logs": tx.receipt.logs.iter().map(log_json).collect::<Vec<_>>(),
        "logsBloom": bloom(&tx.receipt.logs_bloom),
        "root": ZERO_32,
        "status": if tx.receipt.success { "0x1" } else { "0x0" },
        "effectiveGasPrice": weibar_u64(tx.receipt.effective_gas_price),
    })
}

/// Log object.
pub fn log_json(log: &StoredLog) -> Value {
    json!({
        "address": address(&log.address),
        "topics": log.topics.iter().map(hash).collect::<Vec<_>>(),
        "data": data(&log.data),
        "blockNumber": quantity_u64(log.block_number),
        "blockHash": hash(&log.block_hash),
        "transactionHash": hash(&log.tx_hash),
        "transactionIndex": quantity_u64(log.tx_index),
        "logIndex": quantity_u64(log.log_index),
        "removed": false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantities_have_no_leading_zeros() {
        assert_eq!(quantity_u64(0), "0x0");
        assert_eq!(quantity_u64(298), "0x12a");
        assert_eq!(quantity(U256::from(255u64)), "0xff");
    }

    #[test]
    fn tinybar_renders_as_weibar() {
        assert_eq!(weibar(Tinybar(1)), "0x2540be400");
        assert_eq!(weibar_u64(71), "0xa54f4c3c00");
    }

    #[test]
    fn call_object_converts_value_and_prefers_input() {
        let call = parse_call(Some(&json!({
            "to": "0x0000000000000000000000000000000000000001",
            "value": "0x2540be400",
            "data": "0x01",
            "input": "0x02",
            "gas": "0x5208",
        })))
        .expect("parses");
        assert_eq!(call.value, 1);
        assert_eq!(call.input.as_ref(), &[0x02]);
        assert_eq!(call.gas, Some(21_000));
        let fractional = parse_call(Some(&json!({ "value": "0x1" })));
        assert!(fractional.is_err());
    }
}
