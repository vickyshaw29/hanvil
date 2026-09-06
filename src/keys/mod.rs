//! Key material: predefined dev accounts and public-key derivation.

pub mod predefined;

use alloy_primitives::{Address, keccak256};

use crate::state::Key;

/// Errors deriving keys.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Hex private key did not decode.
    #[error("private key is not valid hex: {0}")]
    Hex(#[from] hex::FromHexError),
    /// Wrong length or not a valid scalar.
    #[error("private key rejected: {0}")]
    Invalid(String),
}

/// secp256k1 public key (compressed) and EVM alias for a 32-byte private key.
pub fn ecdsa_public(private: &[u8]) -> Result<(Key, Address), Error> {
    let signing =
        k256::ecdsa::SigningKey::from_slice(private).map_err(|e| Error::Invalid(e.to_string()))?;
    let verifying = signing.verifying_key();
    let compressed = verifying.to_encoded_point(true);
    let mut key = [0u8; 33];
    key.copy_from_slice(compressed.as_bytes());
    let uncompressed = verifying.to_encoded_point(false);
    let alias = Address::from_slice(&keccak256(&uncompressed.as_bytes()[1..])[12..]);
    Ok((Key::EcdsaSecp256k1(key), alias))
}

/// Ed25519 public key for a 32-byte private key.
pub fn ed25519_public(private: &[u8]) -> Result<Key, Error> {
    let bytes: [u8; 32] = private
        .try_into()
        .map_err(|_| Error::Invalid("ed25519 private key must be 32 bytes".into()))?;
    let signing = ed25519_dalek::SigningKey::from_bytes(&bytes);
    Ok(Key::Ed25519(signing.verifying_key().to_bytes()))
}

/// Strip an optional `0x` and decode hex.
pub fn decode_hex(text: &str) -> Result<Vec<u8>, Error> {
    Ok(hex::decode(text.strip_prefix("0x").unwrap_or(text))?)
}
