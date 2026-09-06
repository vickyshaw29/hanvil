//! Wire encodings: hex quantities and parameter parsing.

use alloy_primitives::{Address, U256};
use serde_json::Value;

use super::RpcError;

/// `0x`-prefixed minimal hex, as every Ethereum QUANTITY is encoded. Zero is `0x0`.
pub fn quantity(value: U256) -> String {
    format!("0x{value:x}")
}

/// [`quantity`] for a `u64` (chain id, block number, nonce, gas).
pub fn quantity_u64(value: u64) -> String {
    quantity(U256::from(value))
}

/// A 20-byte address parameter.
pub fn parse_address(param: Option<&Value>) -> Result<Address, RpcError> {
    let text = param
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params("address must be a hex string"))?;
    text.parse::<Address>()
        .map_err(|_| RpcError::invalid_params(format!("`{text}` is not a 20-byte hex address")))
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
}
