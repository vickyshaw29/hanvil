//! Anvil's cheat methods (`evm_*`, `anvil_*`) and their `hardhat_*` aliases. Return values match
//! foundry `crates/anvil/src/eth/api.rs` so scripts written for Anvil run unchanged.

use serde_json::{Value, json};

use super::RpcError;
use super::types::{
    hash, parse_address, parse_bytes, parse_quantity, parse_u64, quantity_u64, weibar_u64,
};
use crate::evm::units::Tinybar;
use crate::state::{BLOCK_GAS_LIMIT, Chain, Timestamp};

/// Whether this module answers `method`.
pub fn handles(method: &str) -> bool {
    method.starts_with("evm_") || method.starts_with("anvil_") || method.starts_with("hardhat_")
}

/// Dispatch one cheat.
pub fn call(
    chain: &mut Chain,
    now: Timestamp,
    method: &str,
    params: &[Value],
) -> Result<Value, RpcError> {
    let p = |i: usize| params.get(i);
    let canonical = method
        .strip_prefix("hardhat_")
        .map(|rest| format!("anvil_{rest}"))
        .unwrap_or_else(|| method.to_string());
    match canonical.as_str() {
        "evm_snapshot" => Ok(json!(quantity_u64(chain.snapshot()))),
        "evm_revert" => Ok(json!(chain.revert(parse_u64(p(0), "snapshot id")?))),
        "evm_mine" | "anvil_mine" => {
            let (count, pinned) = mine_params(&canonical, params)?;
            if let Some(timestamp) = pinned {
                chain.set_next_timestamp(timestamp)?;
            }
            for _ in 0..count {
                chain.mine_empty(now);
            }
            Ok(if canonical == "evm_mine" {
                json!("0x0")
            } else {
                Value::Null
            })
        }
        "evm_increaseTime" => {
            let seconds = parse_u64(p(0), "seconds")?;
            Ok(json!(chain.increase_time(seconds)))
        }
        "evm_setNextBlockTimestamp" => {
            chain.set_next_timestamp(parse_u64(p(0), "timestamp")?)?;
            Ok(Value::Null)
        }
        "anvil_setBalance" => {
            let address = parse_address(p(0))?;
            let balance = Tinybar::from_weibar_exact(parse_quantity(p(1), "balance")?)
                .map_err(|e| RpcError::invalid_params(e.to_string()))?;
            chain.set_balance(address, balance);
            Ok(Value::Null)
        }
        "anvil_setCode" => {
            chain.set_code(parse_address(p(0))?, parse_bytes(p(1), "code")?);
            Ok(Value::Null)
        }
        "anvil_setNonce" => {
            chain.set_nonce(parse_address(p(0))?, parse_u64(p(1), "nonce")?);
            Ok(Value::Null)
        }
        "anvil_setStorageAt" => {
            chain.set_storage(
                parse_address(p(0))?,
                parse_quantity(p(1), "slot")?,
                parse_quantity(p(2), "value")?,
            );
            Ok(json!(true))
        }
        "anvil_impersonateAccount" => {
            chain.impersonate(parse_address(p(0))?);
            Ok(Value::Null)
        }
        "anvil_stopImpersonatingAccount" => {
            chain.stop_impersonating(parse_address(p(0))?);
            Ok(Value::Null)
        }
        "anvil_nodeInfo" => {
            let head = chain.latest_block();
            Ok(json!({
                "currentBlockNumber": quantity_u64(head.number),
                "currentBlockTimestamp": head.timestamp,
                "currentBlockHash": hash(&head.hash),
                "hardFork": "cancun",
                "transactionOrder": "fifo",
                "environment": {
                    "baseFee": weibar_u64(chain.gas_price().0),
                    "chainId": chain.chain_id(),
                    "gasLimit": quantity_u64(BLOCK_GAS_LIMIT),
                    "gasPrice": weibar_u64(chain.gas_price().0),
                },
                "forkConfig": {},
            }))
        }
        "evm_setAutomine"
        | "anvil_setAutomine"
        | "evm_setIntervalMining"
        | "anvil_setIntervalMining" => Err(RpcError::unsupported_with_reason(
            method,
            "hanvil mines one block per transaction; batch mining is not emulated",
        )),
        "anvil_dumpState" | "anvil_loadState" | "anvil_reset" => Err(
            RpcError::unsupported_with_reason(method, "not emulated yet; use evm_snapshot"),
        ),
        _ => Err(RpcError::method_not_found(method)),
    }
}

/// `evm_mine` takes an optional `{timestamp}` (or a bare timestamp); `anvil_mine` takes an
/// optional block count and interval.
fn mine_params(canonical: &str, params: &[Value]) -> Result<(u64, Option<u64>), RpcError> {
    if canonical == "anvil_mine" {
        let count = match params.first() {
            None | Some(Value::Null) => 1,
            some => parse_u64(some, "blocks")?,
        };
        return Ok((count, None));
    }
    match params.first() {
        None | Some(Value::Null) => Ok((1, None)),
        Some(Value::Object(opts)) => match opts.get("timestamp") {
            None | Some(Value::Null) => Ok((1, None)),
            some => Ok((1, Some(parse_u64(some, "timestamp")?))),
        },
        some => Ok((1, Some(parse_u64(some, "timestamp")?))),
    }
}
