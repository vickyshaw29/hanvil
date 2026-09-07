//! Accounts and their identities.

use std::fmt;

use alloy_primitives::Address;
use serde::{Deserialize, Serialize};

use super::time::Timestamp;
use crate::evm::units::{Tinybar, long_zero_address};

/// Hedera entity id in shard 0, realm 0: `0.0.N`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EntityId(pub u64);

impl fmt::Display for EntityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0.0.{}", self.0)
    }
}

/// An account's signing key. Only single keys in v0; key lists are not emulated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Key {
    /// Compressed secp256k1 public key, 33 bytes.
    EcdsaSecp256k1([u8; 33]),
    /// Ed25519 public key, 32 bytes.
    Ed25519([u8; 32]),
}

impl Serialize for Key {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let text = match self {
            Key::EcdsaSecp256k1(b) => format!("ecdsa:{}", hex::encode(b)),
            Key::Ed25519(b) => format!("ed25519:{}", hex::encode(b)),
        };
        s.serialize_str(&text)
    }
}

impl<'de> Deserialize<'de> for Key {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        let (kind, body) = text
            .split_once(':')
            .ok_or_else(|| serde::de::Error::custom("key must be `kind:hex`"))?;
        let bytes = hex::decode(body).map_err(serde::de::Error::custom)?;
        match kind {
            "ecdsa" => bytes
                .try_into()
                .map(Key::EcdsaSecp256k1)
                .map_err(|_| serde::de::Error::custom("ecdsa key must be 33 bytes")),
            "ed25519" => bytes
                .try_into()
                .map(Key::Ed25519)
                .map_err(|_| serde::de::Error::custom("ed25519 key must be 32 bytes")),
            other => Err(serde::de::Error::custom(format!(
                "unknown key kind `{other}`"
            ))),
        }
    }
}

/// A Hedera account.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Account {
    /// Entity id.
    pub id: EntityId,
    /// Signing key. `None` for hollow accounts (created by a transfer to an unknown EVM
    /// address) and for the system accounts Hanvil never signs for.
    pub key: Option<Key>,
    /// EVM alias (keccak of the public key) when the account was created with one.
    pub alias: Option<Address>,
    /// Balance in tinybar. Authoritative; the EVM view is derived from this.
    pub balance: Tinybar,
    /// Ethereum-style nonce, incremented by EVM transactions.
    pub nonce: u64,
    /// Set by CryptoDelete.
    pub deleted: bool,
    /// Account memo.
    pub memo: String,
    /// When the account came into being. Genesis accounts carry the boot time; an account created
    /// by a transfer carries that transaction's consensus timestamp. The mirror reports it as
    /// `created_timestamp` (`openapi.yml:2004`).
    pub created_at: Timestamp,
    /// Private key, kept only for predefined dev accounts so the banner can print it.
    pub private_key_hex: Option<String>,
}

impl Account {
    /// The address the EVM sees: the alias when present, else the long-zero form of the id.
    pub fn evm_address(&self) -> Address {
        self.alias.unwrap_or_else(|| long_zero_address(self.id))
    }
}
