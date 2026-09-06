//! Units and address forms. The only module allowed to convert between them.
//!
//! HBAR has 8 decimals (tinybar); JSON-RPC exposes 18 (weibar): 1 tinybar = 10^10 wei.
//! Long-zero addresses are shard(4) ‖ realm(8) ‖ num(8), big-endian.

use alloy_primitives::{Address, U256};
use serde::{Deserialize, Serialize};

use crate::state::EntityId;

/// Tinybar per HBAR.
pub const TINYBAR_PER_HBAR: u64 = 100_000_000;
/// Wei per tinybar, as the JSON-RPC relay reports balances and values.
pub const WEIBAR_PER_TINYBAR: u64 = 10_000_000_000;

/// Amount in tinybar.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct Tinybar(pub u64);

impl Tinybar {
    /// Whole HBAR to tinybar. Saturates rather than wrapping.
    pub const fn from_hbar(hbar: u64) -> Self {
        Self(hbar.saturating_mul(TINYBAR_PER_HBAR))
    }

    /// Value as the EVM sees it (18 decimals).
    pub fn to_weibar(self) -> U256 {
        U256::from(self.0) * U256::from(WEIBAR_PER_TINYBAR)
    }
}

/// Long-zero address for an entity id.
pub fn long_zero_address(id: EntityId) -> Address {
    let mut bytes = [0u8; 20];
    bytes[12..].copy_from_slice(&id.0.to_be_bytes());
    Address::from(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hbar_to_tinybar_to_weibar() {
        let t = Tinybar::from_hbar(10_000);
        assert_eq!(t.0, 1_000_000_000_000);
        assert_eq!(
            t.to_weibar(),
            U256::from(10_000u64) * U256::from(10u64).pow(U256::from(18))
        );
    }

    #[test]
    fn long_zero_round_trip() {
        let id = EntityId(1002);
        let addr = long_zero_address(id);
        assert_eq!(
            format!("{addr:?}"),
            "0x00000000000000000000000000000000000003ea"
        );
    }
}
