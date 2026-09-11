//! Anvil's cheat methods (`evm_*`, `anvil_*`) and their `hardhat_*` aliases. Return values match
//! foundry `crates/anvil/src/eth/api.rs` so scripts written for Anvil run unchanged.
//!
//! Plus one method of Hanvil's own, under its own prefix: `hanvil_rejections`. It is not an Anvil
//! method and is not pretending to be one, which is why it is not in the `anvil_` namespace.

use serde_json::{Value, json};

use super::RpcError;
use super::types::{
    address as address_hex, hash, parse_address, parse_bytes, parse_quantity, parse_u64,
    quantity_u64, weibar_u64,
};
use crate::evm::units::Tinybar;
use crate::state::{BLOCK_GAS_LIMIT, Chain, Timestamp};

/// Whether this module answers `method`.
pub fn handles(method: &str) -> bool {
    method.starts_with("evm_")
        || method.starts_with("anvil_")
        || method.starts_with("hardhat_")
        || method.starts_with("hanvil_")
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
            let Mine {
                count,
                pinned,
                interval,
            } = mine_params(&canonical, params)?;
            if let Some(timestamp) = pinned {
                chain.set_next_timestamp(timestamp)?;
            }
            for block in 0..count {
                // Anvil spaces the blocks by `interval` seconds; the first one keeps the time it
                // would have had, so `anvil_mine(3, 30)` covers a minute, not a minute and a half.
                if block > 0 {
                    chain.increase_time(interval);
                }
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
            chain.set_balance(address, balance, now);
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
        // Hanvil's own. A transaction refused before consensus leaves no record, no receipt and
        // no mirror row — on Hedera and here alike — so without this the error returned to the
        // caller is the only trace it ever existed. Takes an optional cap; the list grows for
        // the life of the chain, as `hapi_records` does.
        "hanvil_rejections" => {
            let limit = match p(0) {
                None | Some(Value::Null) => usize::MAX,
                some => parse_u64(some, "limit")? as usize,
            };
            let kept: Vec<&crate::state::Rejection> = chain.rejections().collect();
            let from = kept.len().saturating_sub(limit);
            Ok(Value::Array(
                kept[from..]
                    .iter()
                    .map(|rejection| {
                        json!({
                            "at": rejection.at.to_string(),
                            "kind": rejection.kind_name(),
                            "payer": rejection.payer.map(|id| json!(id.to_string())),
                            "from": rejection.from.map(|address| json!(address_hex(&address))),
                            "code": rejection.status.map(|status| json!(status.code())),
                            "reason": rejection.reason(),
                        })
                    })
                    .collect(),
            ))
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

/// What one `evm_mine` or `anvil_mine` call asks for.
struct Mine {
    /// Blocks to mine.
    count: u64,
    /// A timestamp the next block is pinned to, from `evm_mine`.
    pinned: Option<u64>,
    /// Seconds between the blocks, from `anvil_mine`'s second argument.
    interval: u64,
}

/// `evm_mine` takes an optional `{timestamp}` (or a bare timestamp); `anvil_mine` takes an
/// optional block count and an optional interval in seconds between them.
fn mine_params(canonical: &str, params: &[Value]) -> Result<Mine, RpcError> {
    if canonical == "anvil_mine" {
        let count = match params.first() {
            None | Some(Value::Null) => 1,
            some => parse_u64(some, "blocks")?,
        };
        let interval = match params.get(1) {
            None | Some(Value::Null) => 0,
            some => parse_u64(some, "interval")?,
        };
        return Ok(Mine {
            count,
            pinned: None,
            interval,
        });
    }
    let pinned = match params.first() {
        None | Some(Value::Null) => None,
        Some(Value::Object(opts)) => match opts.get("timestamp") {
            None | Some(Value::Null) => None,
            some => Some(parse_u64(some, "timestamp")?),
        },
        some => Some(parse_u64(some, "timestamp")?),
    };
    Ok(Mine {
        count: 1,
        pinned,
        interval: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{BodyKind, Genesis, Rejection, Status};

    const NOW: Timestamp = Timestamp {
        secs: 1_757_000_000,
        nanos: 6_000_000,
    };

    fn chain() -> Chain {
        Chain::genesis(&Genesis {
            chain_id: 298,
            accounts_per_type: 1,
            balance: Tinybar::from_hbar(10_000),
            gas_price: Tinybar(71),
            now: NOW,
        })
        .expect("genesis")
    }

    /// The only place a caller can read what the node would not take. Nothing else on any Hedera
    /// wire carries it.
    #[test]
    fn hanvil_rejections_answers_oldest_first_and_honours_a_cap() {
        let mut chain = chain();
        assert_eq!(
            call(&mut chain, NOW, "hanvil_rejections", &[]).expect("empty"),
            json!([])
        );

        chain.reject(Rejection {
            at: NOW,
            kind: Some(BodyKind::ConsensusSubmitMessage),
            payer: Some(crate::state::EntityId(1002)),
            status: Some(Status::InvalidSignature),
            from: None,
            message: String::new(),
        });
        chain.reject(Rejection {
            at: NOW,
            kind: None,
            payer: None,
            status: None,
            from: Some(alloy_primitives::Address::repeat_byte(0xab)),
            message: "1 weibar is not a multiple of 10^10".to_string(),
        });

        let all = call(&mut chain, NOW, "hanvil_rejections", &[]).expect("rows");
        assert_eq!(
            all,
            json!([
                {
                    "at": "1757000000.006000000",
                    "kind": "CONSENSUSSUBMITMESSAGE",
                    "payer": "0.0.1002",
                    "from": null,
                    "code": 7,
                    "reason": "INVALID_SIGNATURE",
                },
                {
                    "at": "1757000000.006000000",
                    "kind": "UNKNOWN",
                    "payer": null,
                    "from": "0xabababababababababababababababababababab",
                    "code": null,
                    "reason": "1 weibar is not a multiple of 10^10",
                },
            ])
        );

        // The cap keeps the most recent rows, since those are the ones being debugged.
        let capped = call(&mut chain, NOW, "hanvil_rejections", &[json!(1)]).expect("capped");
        assert_eq!(capped.as_array().expect("array").len(), 1);
        assert_eq!(capped[0]["kind"], "UNKNOWN");
    }

    /// The prefix is routed, but only the one method exists.
    #[test]
    fn another_hanvil_method_is_not_found_rather_than_unrouted() {
        let error = call(&mut chain(), NOW, "hanvil_nothing", &[]).expect_err("no such method");
        assert_eq!(error.code, -32601);
        assert!(handles("hanvil_nothing"), "the prefix is still routed here");
    }
}
