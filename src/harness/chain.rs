//! The chain under the run. `validation/chainSigner.ts` of hedera-harness dev @ 587a2f3 and the
//! fork's `chainSnapshot.ts`, except that here the chain is a struct in this process: the
//! signer is created with `Chain::apply_hapi`, snapshots are `Chain::snapshot`, and assertions
//! read the chain directly.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::harness::artifacts::now_iso8601;

/// `validation/chainSigner.ts:9`.
pub(crate) const SIGNER_FILENAME: &str = "chain-signer.json";

/// `types.ts` `ChainSigner`, as persisted in `chain-signer.json` (mode 0600).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Signer {
    /// `0.0.N`.
    pub(crate) account_id: String,
    /// `0x` + 64 hex.
    pub(crate) private_key_hex: String,
    /// `0x` + 40 hex.
    pub(crate) evm_address: String,
    /// `local`; `testnet` when written by hedera-harness.
    pub(crate) network: String,
    /// Persisted only; `toPublicSigner` drops it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) created_at: Option<String>,
}

impl Signer {
    /// `chainSigner.ts:415-422`: the shape handed to prompts and logs.
    pub(crate) fn public(&self) -> Self {
        Self {
            created_at: None,
            ..self.clone()
        }
    }

    /// `chainSigner.ts:90-102`: write with `createdAt`, readable by the owner only.
    pub(crate) fn write(&self, path: &Path) -> std::io::Result<()> {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        let persisted = Self {
            created_at: Some(self.created_at.clone().unwrap_or_else(now_iso8601)),
            ..self.clone()
        };
        let json = serde_json::to_string_pretty(&persisted).map_err(std::io::Error::other)?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(format!("{json}\n").as_bytes())
    }

    /// `chainSigner.ts:389-405`: a persisted signer, when the file is present and well-formed.
    pub(crate) fn read(path: &Path) -> Option<Self> {
        let raw = std::fs::read_to_string(path).ok()?;
        let signer: Self = serde_json::from_str(&raw).ok()?;
        let hex_ok = |value: &str, len: usize| {
            value
                .strip_prefix("0x")
                .is_some_and(|hex| hex.len() == len && hex.bytes().all(|b| b.is_ascii_hexdigit()))
        };
        (matches!(signer.network.as_str(), "local" | "testnet")
            && hex_ok(&signer.private_key_hex, 64)
            && hex_ok(&signer.evm_address, 40))
        .then_some(signer)
    }
}

/// The endpoints the node under the run listens on, and its chain id. Injected into every
/// subprocess and written into the generator prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocalChain {
    /// JSON-RPC.
    pub(crate) rpc_url: String,
    /// Mirror REST.
    pub(crate) mirror_url: String,
    /// HAPI gRPC, `host:port`.
    pub(crate) grpc_url: String,
    /// EVM chain id.
    pub(crate) chain_id: u64,
}

impl LocalChain {
    /// `HANVIL_*` and `HEDERA_NETWORK=local` for the generator, deploy commands, validator
    /// commands and the dev server. Upstream gives the agent a mirror URL in a prompt and
    /// nothing else.
    pub(crate) fn env(&self) -> BTreeMap<String, String> {
        BTreeMap::from([
            ("HANVIL_RPC_URL".to_string(), self.rpc_url.clone()),
            ("HANVIL_MIRROR_URL".to_string(), self.mirror_url.clone()),
            ("HANVIL_GRPC_URL".to_string(), self.grpc_url.clone()),
            ("HANVIL_CHAIN_ID".to_string(), self.chain_id.to_string()),
            ("HEDERA_NETWORK".to_string(), "local".to_string()),
        ])
    }
}

/// `chainSigner.ts:239-252`: the signer for deploy commands, plus every `expose.envVars`
/// name set to the private key.
pub(crate) fn deploy_env(signer: &Signer, expose_env_vars: &[String]) -> BTreeMap<String, String> {
    let mut env = BTreeMap::from([
        (
            "HARNESS_SIGNER_ACCOUNT_ID".to_string(),
            signer.account_id.clone(),
        ),
        (
            "HARNESS_SIGNER_EVM_ADDRESS".to_string(),
            signer.evm_address.clone(),
        ),
        (
            "HARNESS_SIGNER_PRIVATE_KEY".to_string(),
            signer.private_key_hex.clone(),
        ),
    ]);
    for name in expose_env_vars {
        env.insert(name.clone(), signer.private_key_hex.clone());
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signer() -> Signer {
        Signer {
            account_id: "0.0.1032".into(),
            private_key_hex: format!("0x{}", "ab".repeat(32)),
            evm_address: format!("0x{}", "cd".repeat(20)),
            network: "local".into(),
            created_at: None,
        }
    }

    #[test]
    fn the_signer_file_round_trips_with_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = std::env::temp_dir().join(format!("hanvil-chain-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join(SIGNER_FILENAME);
        signer().write(&path).expect("write");
        assert_eq!(
            std::fs::metadata(&path).expect("meta").permissions().mode() & 0o777,
            0o600
        );
        let raw = std::fs::read_to_string(&path).expect("read");
        assert!(raw.contains("\"createdAt\": \""), "{raw}");
        assert!(raw.contains("\"privateKeyHex\": \"0xabab"), "{raw}");
        let read = Signer::read(&path).expect("parses");
        assert_eq!(read.public(), signer());
        assert!(read.created_at.is_some());
        std::fs::write(
            &path,
            r#"{"accountId":"0.0.1","privateKeyHex":"0x12","evmAddress":"0x34","network":"local"}"#,
        )
        .expect("write");
        assert_eq!(Signer::read(&path), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn env_injection_names_every_endpoint_and_exposed_variable() {
        let env = deploy_env(&signer(), &["DEPLOYER_PRIVATE_KEY".to_string()]);
        assert_eq!(env["HARNESS_SIGNER_ACCOUNT_ID"], "0.0.1032");
        assert_eq!(
            env["DEPLOYER_PRIVATE_KEY"],
            env["HARNESS_SIGNER_PRIVATE_KEY"]
        );
        let local = LocalChain {
            rpc_url: "http://127.0.0.1:7546".into(),
            mirror_url: "http://127.0.0.1:5551".into(),
            grpc_url: "127.0.0.1:50211".into(),
            chain_id: 298,
        }
        .env();
        assert_eq!(local["HEDERA_NETWORK"], "local");
        assert_eq!(local["HANVIL_CHAIN_ID"], "298");
        assert_eq!(local["HANVIL_GRPC_URL"], "127.0.0.1:50211");
    }
}
