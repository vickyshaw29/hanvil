//! `eth_*`, `net_*`, `web3_*` methods. Behaviour follows hiero-json-rpc-relay docs/rpc-api.md.

use alloy_primitives::keccak256;
use revm::context_interface::result::ExecutionResult;
use serde_json::{Value, json};

use super::RpcError;
use super::types::{
    block_json, data, hash, log_json, parse_address, parse_block_number, parse_bytes, parse_call,
    parse_hash, parse_log_filter, parse_optional_nonce, parse_quantity, parse_u64, quantity_u64,
    receipt_json, to_block_is_pinned, tx_json, weibar, weibar_u64,
};
use crate::evm;
use crate::state::{BLOCK_GAS_LIMIT, Chain, FilterChanges, Timestamp, UnsignedTx};

/// Methods the relay lists but answers with -32601. Kept identical so tooling that probes
/// capabilities sees the same surface it would on a real relay.
const UNSUPPORTED: &[&str] = &[
    "eth_blobBaseFee",
    // The relay's own summary for it: "Always returns UNSUPPORTED_METHOD error"
    // (`openrpc.json`, method `net_peerCount`).
    "net_peerCount",
    // Hanvil mines one block per transaction, so nothing is ever pending. The relay declares this
    // method unsupported too (`openrpc.json:950-957`).
    "eth_newPendingTransactionFilter",
    "eth_coinbase",
    "eth_createAccessList",
    "eth_getProof",
    "eth_protocolVersion",
    "eth_sign",
    "eth_signTransaction",
    "eth_signTypedData",
    "eth_getWork",
    "eth_submitHashrate",
];

/// Dispatch one method. Every method takes the write lock: `eth_call` runs the EVM in place.
pub fn call(
    chain: &mut Chain,
    now: Timestamp,
    method: &str,
    params: &[Value],
) -> Result<Value, RpcError> {
    if UNSUPPORTED.contains(&method) {
        return Err(RpcError::unsupported(method));
    }
    let p = |i: usize| params.get(i);
    match method {
        "eth_chainId" => Ok(json!(quantity_u64(chain.chain_id()))),
        "net_version" => Ok(json!(chain.chain_id().to_string())),
        "net_listening" => Ok(json!(true)),
        "web3_clientVersion" => Ok(json!(format!("hanvil/{}", env!("CARGO_PKG_VERSION")))),
        "web3_sha3" => Ok(json!(hash(&keccak256(parse_bytes(p(0), "data")?)))),
        "eth_blockNumber" => Ok(json!(quantity_u64(chain.block_number()))),
        "eth_gasPrice" => Ok(json!(weibar(chain.gas_price()))),
        "eth_maxPriorityFeePerGas" => Ok(json!("0x0")),
        "eth_feeHistory" => fee_history(chain, params),
        "eth_accounts" => Ok(json!([])),
        "eth_mining" => Ok(json!(false)),
        "eth_syncing" => Ok(json!(false)),
        "eth_hashrate" => Ok(json!("0x0")),
        "eth_submitWork" => Ok(json!(false)),
        "eth_getUncleByBlockHashAndIndex" | "eth_getUncleByBlockNumberAndIndex" => Ok(Value::Null),
        "eth_getUncleCountByBlockHash" | "eth_getUncleCountByBlockNumber" => Ok(json!("0x0")),

        "eth_getBalance" => {
            let address = parse_address(p(0))?;
            require_head_state(chain, p(1))?;
            Ok(json!(weibar(chain.balance_by_evm(&address))))
        }
        "eth_getTransactionCount" => {
            let address = parse_address(p(0))?;
            require_head_state(chain, p(1))?;
            Ok(json!(quantity_u64(chain.nonce_by_evm(&address))))
        }
        "eth_getCode" => {
            let address = parse_address(p(0))?;
            require_head_state(chain, p(1))?;
            Ok(json!(data(&chain.code_by_evm(&address))))
        }
        "eth_getStorageAt" => {
            let address = parse_address(p(0))?;
            let slot = parse_quantity(p(1), "slot")?;
            require_head_state(chain, p(2))?;
            Ok(json!(hash(&chain.storage_at(&address, slot).into())))
        }

        "eth_getBlockByNumber" => {
            let number = match parse_block_number(chain, p(0)) {
                Ok(n) => n,
                // A number past the head is "no such block", not an error.
                Err(_)
                    if p(0)
                        .and_then(Value::as_str)
                        .is_some_and(|s| s.starts_with("0x")) =>
                {
                    return Ok(Value::Null);
                }
                Err(e) => return Err(e),
            };
            let full = p(1).and_then(Value::as_bool).unwrap_or(false);
            Ok(chain
                .block_by_number(number)
                .map(|b| block_json(chain, b, full))
                .unwrap_or(Value::Null))
        }
        "eth_getBlockByHash" => {
            let hash = parse_hash(p(0))?;
            let full = p(1).and_then(Value::as_bool).unwrap_or(false);
            Ok(chain
                .block_by_hash(&hash)
                .map(|b| block_json(chain, b, full))
                .unwrap_or(Value::Null))
        }
        "eth_getBlockTransactionCountByNumber" => {
            let number = parse_block_number(chain, p(0))?;
            Ok(chain
                .block_by_number(number)
                .map(|b| json!(quantity_u64(b.transactions.len() as u64)))
                .unwrap_or(Value::Null))
        }
        "eth_getBlockTransactionCountByHash" => {
            let hash = parse_hash(p(0))?;
            Ok(chain
                .block_by_hash(&hash)
                .map(|b| json!(quantity_u64(b.transactions.len() as u64)))
                .unwrap_or(Value::Null))
        }
        "eth_getBlockReceipts" => {
            let number = parse_block_number(chain, p(0))?;
            Ok(chain
                .block_by_number(number)
                .map(|b| {
                    Value::Array(
                        b.transactions
                            .iter()
                            .filter_map(|h| chain.transaction(h))
                            .map(receipt_json)
                            .collect(),
                    )
                })
                .unwrap_or(Value::Null))
        }
        "eth_getTransactionByHash" => {
            let hash = parse_hash(p(0))?;
            Ok(chain.transaction(&hash).map(tx_json).unwrap_or(Value::Null))
        }
        "eth_getTransactionReceipt" => {
            let hash = parse_hash(p(0))?;
            Ok(chain
                .transaction(&hash)
                .map(receipt_json)
                .unwrap_or(Value::Null))
        }
        "eth_getTransactionByBlockNumberAndIndex" => {
            let number = parse_block_number(chain, p(0))?;
            let index = parse_u64(p(1), "index")?;
            Ok(tx_at(chain, chain.block_by_number(number), index))
        }
        "eth_getTransactionByBlockHashAndIndex" => {
            let hash = parse_hash(p(0))?;
            let index = parse_u64(p(1), "index")?;
            Ok(tx_at(chain, chain.block_by_hash(&hash), index))
        }
        "eth_getLogs" => {
            let filter = parse_log_filter(chain, p(0))?;
            Ok(Value::Array(
                chain.logs(&filter).into_iter().map(log_json).collect(),
            ))
        }

        "eth_newFilter" => {
            let mut query = parse_log_filter(chain, p(0))?;
            if !to_block_is_pinned(p(0)) {
                query.to_block = u64::MAX;
            }
            Ok(json!(quantity_u64(chain.install_log_filter(query))))
        }
        "eth_newBlockFilter" => Ok(json!(quantity_u64(chain.install_block_filter()))),
        "eth_uninstallFilter" => {
            let id = parse_u64(p(0), "filter id")?;
            Ok(json!(chain.uninstall_filter(id)))
        }
        "eth_getFilterChanges" => {
            let id = parse_u64(p(0), "filter id")?;
            Ok(match chain.filter_changes(id)? {
                FilterChanges::Logs(logs) => Value::Array(logs.iter().map(log_json).collect()),
                FilterChanges::Blocks(hashes) => {
                    Value::Array(hashes.iter().map(|h| json!(hash(h))).collect())
                }
            })
        }
        "eth_getFilterLogs" => {
            let id = parse_u64(p(0), "filter id")?;
            Ok(Value::Array(
                chain.filter_logs(id)?.into_iter().map(log_json).collect(),
            ))
        }

        "eth_call" => {
            let request = parse_call(p(0))?;
            require_head_state(chain, p(1))?;
            let result = chain.call(&request, now)?;
            match result {
                ExecutionResult::Success { output, .. } => Ok(json!(data(output.data()))),
                other => Err(RpcError::from_failed_execution(&other)),
            }
        }
        "eth_estimateGas" => {
            let request = parse_call(p(0))?;
            require_head_state(chain, p(1))?;
            // Estimation reports reverts the way eth_call does, so tooling can show the reason.
            let probe = chain.call(&request, now)?;
            if !probe.is_success() {
                return Err(RpcError::from_failed_execution(&probe));
            }
            Ok(json!(quantity_u64(chain.estimate_gas(&request, now)?)))
        }
        "eth_sendRawTransaction" => {
            let raw = parse_bytes(p(0), "transaction")?;
            Ok(json!(hash(&chain.send_raw(raw, now)?)))
        }
        "eth_sendTransaction" => {
            let request = parse_call(p(0))?;
            let from = request
                .from
                .ok_or_else(|| RpcError::invalid_params("eth_sendTransaction needs `from`"))?;
            let nonce = parse_optional_nonce(p(0))?.unwrap_or_else(|| chain.nonce_by_evm(&from));
            let tx = UnsignedTx {
                from,
                to: request.to,
                nonce,
                gas_limit: request.gas.unwrap_or(BLOCK_GAS_LIMIT / 2),
                gas_price: request.gas_price.unwrap_or(chain.gas_price().0),
                value: request.value,
                input: request.input,
            };
            Ok(json!(hash(&chain.send_unsigned(tx, now)?)))
        }
        other => Err(RpcError::method_not_found(other)),
    }
}

/// Hanvil keeps only the head state. A historical block tag is refused rather than silently
/// answered from the wrong state.
fn require_head_state(chain: &Chain, tag: Option<&Value>) -> Result<(), RpcError> {
    let number = parse_block_number(chain, tag)?;
    if number == chain.block_number() {
        Ok(())
    } else {
        Err(RpcError::server(format!(
            "historical state is not kept: block {number} requested, head is {}; use latest or evm_snapshot",
            chain.block_number()
        )))
    }
}

fn tx_at(chain: &Chain, block: Option<&crate::state::Block>, index: u64) -> Value {
    block
        .and_then(|b| b.transactions.get(usize::try_from(index).ok()?))
        .and_then(|h| chain.transaction(h))
        .map(tx_json)
        .unwrap_or(Value::Null)
}

/// `eth_feeHistory`: the network gas price for every block, zero priority fees.
fn fee_history(chain: &Chain, params: &[Value]) -> Result<Value, RpcError> {
    let count = parse_u64(params.first(), "blockCount")?.clamp(1, 1024);
    let newest = parse_block_number(chain, params.get(1))?;
    let oldest = newest.saturating_sub(count - 1);
    let mut base_fees = Vec::new();
    let mut ratios = Vec::new();
    for number in oldest..=newest {
        if let Some(block) = chain.block_by_number(number) {
            base_fees.push(json!(weibar_u64(block.base_fee)));
            ratios.push(json!(block.gas_used as f64 / block.gas_limit as f64));
        }
    }
    // One more entry than blocks: the next block's base fee, unchanged on a flat-price chain.
    base_fees.push(json!(weibar(chain.gas_price())));
    let mut history = json!({
        "oldestBlock": quantity_u64(oldest),
        "baseFeePerGas": base_fees,
        "gasUsedRatio": ratios,
    });
    if let Some(Value::Array(percentiles)) = params.get(2) {
        let rewards: Vec<Value> = (oldest..=newest)
            .map(|_| Value::Array(percentiles.iter().map(|_| json!("0x0")).collect()))
            .collect();
        history["reward"] = Value::Array(rewards);
    }
    Ok(history)
}

impl RpcError {
    /// A revert or halt from `eth_call` / `eth_estimateGas`, in the geth shape viem and ethers
    /// decode: code 3, `data` carrying the revert bytes.
    pub fn from_failed_execution(result: &ExecutionResult) -> Self {
        match result {
            ExecutionResult::Revert { output, .. } => {
                let message = match evm::revert_reason(output) {
                    Some(reason) => format!("execution reverted: {reason}"),
                    None => "execution reverted".to_string(),
                };
                Self::execution_reverted(message, data(output))
            }
            ExecutionResult::Halt { reason, .. } => Self::server(format!("{reason:?}")),
            ExecutionResult::Success { .. } => Self::server("unexpected success"),
        }
    }
}
