//! Signature verification for HAPI transaction bodies.
//!
//! ECDSA secp256k1 signs `keccak256(bodyBytes)` and the pair carries 64 bytes of r‖s
//! (`hiero-sdk-js/packages/cryptography/src/primitive/ecdsa.js:77-83`). ED25519 signs `bodyBytes`
//! directly. In both cases `pubKeyPrefix` is the full public key as the SDK sends it
//! (`hiero-sdk-js/src/PublicKey.js:263-274`), but the protocol allows any prefix, so a prefix
//! match is what is checked.

use alloy_primitives::keccak256;

use crate::state::Key;

/// One entry of a `SignatureMap`, with the protobuf types left behind.
pub struct SignaturePair<'a> {
    /// A prefix of the signing public key, often the whole key.
    pub prefix: &'a [u8],
    /// The signature bytes and which curve produced them.
    pub signature: Signature<'a>,
}

/// The signature algorithms Hanvil verifies. A `contract`, `rsa3072` or `ecdsa384` pair is
/// neither — those never match a key Hanvil can hold.
pub enum Signature<'a> {
    /// 64 bytes of r‖s over `keccak256(bodyBytes)`.
    EcdsaSecp256k1(&'a [u8]),
    /// 64 bytes over `bodyBytes`.
    Ed25519(&'a [u8]),
}

/// A private key the harness signs with when it is a client of somebody else's network.
///
/// The node never signs anything — it verifies. This exists for `hanvil run` on
/// `chainValidation.network: testnet`, where Hanvil is the one submitting.
#[derive(Clone)]
pub enum Signer {
    /// secp256k1, which is what `portal.hedera.com` issues and what the harness expects.
    EcdsaSecp256k1(Box<k256::ecdsa::SigningKey>),
    /// ed25519, for an operator that has one.
    Ed25519(Box<ed25519_dalek::SigningKey>),
}

impl std::fmt::Debug for Signer {
    /// Never renders the scalar. A key that reaches a log or a prompt is a key that leaked.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::EcdsaSecp256k1(_) => "Signer::EcdsaSecp256k1(<redacted>)",
            Self::Ed25519(_) => "Signer::Ed25519(<redacted>)",
        })
    }
}

impl Signer {
    /// A signer from raw private key bytes: 32 bytes either way, the curve chosen by the caller.
    pub fn from_bytes(bytes: &[u8], ed25519: bool) -> Option<Self> {
        let scalar: [u8; 32] = bytes.try_into().ok()?;
        Some(if ed25519 {
            Self::Ed25519(Box::new(ed25519_dalek::SigningKey::from_bytes(&scalar)))
        } else {
            Self::EcdsaSecp256k1(Box::new(k256::ecdsa::SigningKey::from_slice(&scalar).ok()?))
        })
    }

    /// The public key, in the form a `SignatureMap` prefix and an account's key both use.
    pub fn public(&self) -> Key {
        match self {
            Self::EcdsaSecp256k1(signing) => Key::EcdsaSecp256k1(
                signing
                    .verifying_key()
                    .to_encoded_point(true)
                    .as_bytes()
                    .try_into()
                    .unwrap_or([0; 33]),
            ),
            Self::Ed25519(signing) => Key::Ed25519(signing.verifying_key().to_bytes()),
        }
    }

    /// Sign a transaction body the way the network checks it: ECDSA over `keccak256(bodyBytes)`,
    /// ED25519 over `bodyBytes` itself. The inverse of [`signed_by`], and tested against it.
    pub fn sign(&self, body_bytes: &[u8]) -> [u8; 64] {
        match self {
            Self::EcdsaSecp256k1(signing) => {
                use k256::ecdsa::signature::hazmat::PrehashSigner as _;
                let signature: k256::ecdsa::Signature =
                    match signing.sign_prehash(keccak256(body_bytes).as_slice()) {
                        Ok(signature) => signature,
                        // `sign_prehash` only fails on a prehash that is not 32 bytes; keccak256 is.
                        Err(_) => return [0; 64],
                    };
                signature.to_bytes().into()
            }
            Self::Ed25519(signing) => {
                use ed25519_dalek::Signer as _;
                signing.sign(body_bytes).to_bytes()
            }
        }
    }

    /// Whether this is an ed25519 signer, which decides the `SignaturePair` variant.
    pub fn is_ed25519(&self) -> bool {
        matches!(self, Self::Ed25519(_))
    }
}

/// Whether `pairs` contains a signature by `key` over `body_bytes`.
pub fn signed_by(key: &Key, body_bytes: &[u8], pairs: &[SignaturePair<'_>]) -> bool {
    match key {
        Key::EcdsaSecp256k1(public) => pairs.iter().any(|pair| match pair.signature {
            Signature::EcdsaSecp256k1(bytes) => {
                public.starts_with(pair.prefix) && ecdsa_verifies(public, body_bytes, bytes)
            }
            Signature::Ed25519(_) => false,
        }),
        Key::Ed25519(public) => pairs.iter().any(|pair| match pair.signature {
            Signature::Ed25519(bytes) => {
                public.starts_with(pair.prefix) && ed25519_verifies(public, body_bytes, bytes)
            }
            Signature::EcdsaSecp256k1(_) => false,
        }),
    }
}

/// secp256k1 over the keccak digest of the body. `s` is normalised low before verifying: the SDK
/// produces low-s signatures, and a high-s copy of a valid signature is the same signature.
fn ecdsa_verifies(public: &[u8; 33], body_bytes: &[u8], signature: &[u8]) -> bool {
    use k256::ecdsa::signature::hazmat::PrehashVerifier as _;

    let Ok(verifying) = k256::ecdsa::VerifyingKey::from_sec1_bytes(public) else {
        return false;
    };
    let Ok(parsed) = k256::ecdsa::Signature::from_slice(signature) else {
        return false;
    };
    let normalised = parsed.normalize_s().unwrap_or(parsed);
    verifying
        .verify_prehash(keccak256(body_bytes).as_slice(), &normalised)
        .is_ok()
}

/// ed25519 over the body bytes themselves.
fn ed25519_verifies(public: &[u8; 32], body_bytes: &[u8], signature: &[u8]) -> bool {
    use ed25519_dalek::Verifier as _;

    let Ok(bytes) = <[u8; 64]>::try_from(signature) else {
        return false;
    };
    let Ok(verifying) = ed25519_dalek::VerifyingKey::from_bytes(public) else {
        return false;
    };
    verifying
        .verify(body_bytes, &ed25519_dalek::Signature::from_bytes(&bytes))
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys;

    const BODY: &[u8] = b"a TransactionBody, as far as this test is concerned";

    fn ecdsa_pair() -> (Key, k256::ecdsa::SigningKey) {
        let private = [7u8; 32];
        let (key, _) = keys::ecdsa_public(&private).expect("valid scalar");
        (
            key,
            k256::ecdsa::SigningKey::from_slice(&private).expect("valid scalar"),
        )
    }

    fn sign_ecdsa(signing: &k256::ecdsa::SigningKey, body: &[u8]) -> [u8; 64] {
        use k256::ecdsa::signature::hazmat::PrehashSigner as _;
        let signature: k256::ecdsa::Signature = signing
            .sign_prehash(keccak256(body).as_slice())
            .expect("prehash is 32 bytes");
        signature.to_bytes().into()
    }

    #[test]
    fn ecdsa_signature_over_the_keccak_of_the_body_verifies() {
        let (key, signing) = ecdsa_pair();
        let signature = sign_ecdsa(&signing, BODY);
        let Key::EcdsaSecp256k1(public) = key else {
            unreachable!()
        };
        let pairs = [SignaturePair {
            prefix: &public,
            signature: Signature::EcdsaSecp256k1(&signature),
        }];
        assert!(signed_by(&Key::EcdsaSecp256k1(public), BODY, &pairs));
    }

    #[test]
    fn a_signature_over_other_bytes_does_not_verify() {
        let (key, signing) = ecdsa_pair();
        let signature = sign_ecdsa(&signing, b"a different body");
        let Key::EcdsaSecp256k1(public) = key else {
            unreachable!()
        };
        let pairs = [SignaturePair {
            prefix: &public,
            signature: Signature::EcdsaSecp256k1(&signature),
        }];
        assert!(!signed_by(&Key::EcdsaSecp256k1(public), BODY, &pairs));
    }

    #[test]
    fn a_four_byte_prefix_is_enough_to_select_the_pair() {
        let (key, signing) = ecdsa_pair();
        let signature = sign_ecdsa(&signing, BODY);
        let Key::EcdsaSecp256k1(public) = key else {
            unreachable!()
        };
        let pairs = [SignaturePair {
            prefix: &public[..4],
            signature: Signature::EcdsaSecp256k1(&signature),
        }];
        assert!(signed_by(&Key::EcdsaSecp256k1(public), BODY, &pairs));
    }

    /// The signer is the inverse of the verifier, on both curves. If these ever disagree the
    /// harness would sign transactions its own node would reject.
    #[test]
    fn what_the_signer_produces_is_what_the_verifier_accepts() {
        for ed25519 in [false, true] {
            let signer = Signer::from_bytes(&[9u8; 32], ed25519).expect("32 bytes");
            let public = signer.public();
            let signature = signer.sign(BODY);
            let prefix = match &public {
                Key::EcdsaSecp256k1(bytes) => bytes.to_vec(),
                Key::Ed25519(bytes) => bytes.to_vec(),
            };
            let pair = SignaturePair {
                prefix: &prefix,
                signature: if ed25519 {
                    Signature::Ed25519(&signature)
                } else {
                    Signature::EcdsaSecp256k1(&signature)
                },
            };
            assert!(signed_by(&public, BODY, &[pair]), "ed25519={ed25519}");

            // And it does not accept a different body.
            let pair = SignaturePair {
                prefix: &prefix,
                signature: if ed25519 {
                    Signature::Ed25519(&signature)
                } else {
                    Signature::EcdsaSecp256k1(&signature)
                },
            };
            assert!(!signed_by(&public, b"a different body", &[pair]));
        }
    }

    /// A key that reaches a log or a prompt is a key that leaked.
    #[test]
    fn a_signer_never_renders_its_scalar() {
        let signer = Signer::from_bytes(&[9u8; 32], false).expect("32 bytes");
        let rendered = format!("{signer:?}");
        assert_eq!(rendered, "Signer::EcdsaSecp256k1(<redacted>)");
        assert!(!rendered.contains('9'), "{rendered}");
    }

    #[test]
    fn ed25519_signs_the_body_itself() {
        let private = [3u8; 32];
        let signing = ed25519_dalek::SigningKey::from_bytes(&private);
        let key = keys::ed25519_public(&private).expect("32 bytes");
        let Key::Ed25519(public) = key else {
            unreachable!()
        };
        let signature = {
            use ed25519_dalek::Signer as _;
            signing.sign(BODY).to_bytes()
        };
        let pairs = [SignaturePair {
            prefix: &public,
            signature: Signature::Ed25519(&signature),
        }];
        assert!(signed_by(&Key::Ed25519(public), BODY, &pairs));
        assert!(!signed_by(&Key::Ed25519(public), b"other", &pairs));
    }

    /// An ECDSA pair must not satisfy an ED25519 key even when the prefix happens to match.
    #[test]
    fn the_curve_has_to_match() {
        let (_, signing) = ecdsa_pair();
        let signature = sign_ecdsa(&signing, BODY);
        let key = keys::ed25519_public(&[3u8; 32]).expect("32 bytes");
        let Key::Ed25519(public) = key else {
            unreachable!()
        };
        let pairs = [SignaturePair {
            prefix: &public,
            signature: Signature::EcdsaSecp256k1(&signature),
        }];
        assert!(!signed_by(&Key::Ed25519(public), BODY, &pairs));
    }
}
