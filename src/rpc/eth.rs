//! `eth_*`, `net_*`, `web3_*` methods. Behaviour follows hiero-json-rpc-relay docs/rpc-api.md.

use serde_json::{Value, json};

use super::RpcError;
use super::types::{parse_address, quantity, quantity_u64};
use crate::state::Chain;

/// Methods the relay lists but answers with -32601. Kept identical so tooling that probes
/// capabilities sees the same surface it would on a real relay.
const UNSUPPORTED: &[&str] = &[
    "eth_blobBaseFee",
    "eth_coinbase",
    "eth_createAccessList",
    "eth_getProof",
    "eth_protocolVersion",
    "eth_sendTransaction",
    "eth_sign",
    "eth_signTransaction",
    "eth_signTypedData",
    "eth_getWork",
    "eth_submitHashrate",
];

/// Dispatch one method against a read-locked chain.
pub fn call(chain: &Chain, method: &str, params: &[Value]) -> Result<Value, RpcError> {
    if UNSUPPORTED.contains(&method) {
        return Err(RpcError::unsupported(method));
    }
    match method {
        "eth_chainId" => Ok(json!(quantity_u64(chain.chain_id()))),
        "net_version" => Ok(json!(chain.chain_id().to_string())),
        "eth_blockNumber" => Ok(json!(quantity_u64(chain.block_number()))),
        "eth_getBalance" => {
            let address = parse_address(params.first())?;
            Ok(json!(quantity(chain.balance_by_evm(&address).to_weibar())))
        }
        "eth_accounts" => Ok(json!([])),
        "eth_mining" => Ok(json!(false)),
        "eth_syncing" => Ok(json!(false)),
        "eth_hashrate" => Ok(json!("0x0")),
        "eth_maxPriorityFeePerGas" => Ok(json!("0x0")),
        "net_listening" => Ok(json!(true)),
        "web3_clientVersion" => Ok(json!(format!("hanvil/{}", env!("CARGO_PKG_VERSION")))),
        other => Err(RpcError::method_not_found(other)),
    }
}
