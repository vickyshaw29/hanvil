//! Units and address forms. The only module allowed to convert between them.
//!
//! Inside the EVM one wei is one tinybar, exactly as on Hedera. JSON-RPC exposes 18 decimals
//! (weibar): 1 tinybar = 10^10 weibar. Balances and gas prices are multiplied on the way out;
//! values and gas prices are divided on the way in.
//! Long-zero addresses are shard(4) ‖ realm(8) ‖ num(8), big-endian.

use alloy_primitives::{Address, U256};
use serde::{Deserialize, Serialize};

use crate::state::EntityId;

/// Tinybar per HBAR.
pub const TINYBAR_PER_HBAR: u64 = 100_000_000;
/// Weibar per tinybar, as the JSON-RPC relay reports balances and values.
pub const WEIBAR_PER_TINYBAR: u64 = 10_000_000_000;

/// Amount in tinybar.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct Tinybar(pub u64);

/// A weibar amount that cannot be represented in tinybar.
#[derive(Debug, thiserror::Error)]
pub enum UnitError {
    /// Not a multiple of 10^10.
    #[error("{0} weibar is not a multiple of 10^10 (1 tinybar); the relay rejects such values")]
    NotWhole(U256),
    /// Larger than any tinybar balance.
    #[error("{0} weibar exceeds the tinybar range")]
    TooLarge(U256),
}

impl Tinybar {
    /// Whole HBAR to tinybar. Saturates rather than wrapping.
    pub const fn from_hbar(hbar: u64) -> Self {
        Self(hbar.saturating_mul(TINYBAR_PER_HBAR))
    }

    /// Value as the JSON-RPC client sees it (18 decimals).
    pub fn to_weibar(self) -> U256 {
        U256::from(self.0) * U256::from(WEIBAR_PER_TINYBAR)
    }

    /// Exact conversion from weibar; an error when the value has a fractional tinybar.
    pub fn from_weibar_exact(wei: U256) -> Result<Self, UnitError> {
        let unit = U256::from(WEIBAR_PER_TINYBAR);
        if wei % unit != U256::ZERO {
            return Err(UnitError::NotWhole(wei));
        }
        u64::try_from(wei / unit)
            .map(Self)
            .map_err(|_| UnitError::TooLarge(wei))
    }

    /// Rounded-down conversion from weibar, for gas prices, where the relay floors.
    pub fn from_weibar_floor(wei: U256) -> Result<Self, UnitError> {
        u64::try_from(wei / U256::from(WEIBAR_PER_TINYBAR))
            .map(Self)
            .map_err(|_| UnitError::TooLarge(wei))
    }

    /// Tinybar as the EVM's native unit.
    pub fn to_evm(self) -> U256 {
        U256::from(self.0)
    }

    /// From the EVM's native unit. Saturates: the EVM cannot mint more than exists.
    pub fn from_evm(value: U256) -> Self {
        Self(u64::try_from(value).unwrap_or(u64::MAX))
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
    fn weibar_round_trips_only_when_whole() {
        let one_hbar = Tinybar::from_hbar(1).to_weibar();
        assert_eq!(
            Tinybar::from_weibar_exact(one_hbar).unwrap(),
            Tinybar::from_hbar(1)
        );
        assert!(Tinybar::from_weibar_exact(one_hbar + U256::from(1)).is_err());
        assert_eq!(
            Tinybar::from_weibar_floor(one_hbar + U256::from(1)).unwrap(),
            Tinybar::from_hbar(1)
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
