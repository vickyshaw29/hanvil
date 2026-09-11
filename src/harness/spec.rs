//! Recipe loader. Schema v3 exactly as `hedera-harness` `dev` @ 587a2f3 reads it
//! (`src/specLoader.ts`, `src/specDefaults.ts`), plus `network: local` from PR #47 and three
//! keys under `chainValidation` that only make sense when the harness owns the chain. Every
//! error string is the TypeScript one, so a recipe fails the same way under both harnesses.
//!
//! The YAML is transcoded into `serde_json::Value` at the door, so one `Value` API serves the
//! recipe and the JSON validators it points at.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

/// `specDefaults.ts:12`. Older versions are not loaded.
pub(crate) const SCHEMA_VERSION: u64 = 3;
/// `specDefaults.ts:15-18`.
pub(crate) const DEFAULT_PRD_PATH: &str = ".harness/prd.md";
const DEFAULT_STATIC_VALIDATOR_PATH: &str = ".harness/validators/static.json";
const DEFAULT_COMMANDS_VALIDATOR_PATH: &str = ".harness/validators/yarn.json";
const DEFAULT_MAX_ATTEMPTS: u64 = 3;
/// `specDefaults.ts:24-25`. Not configurable: `.harness/runs/` is the one tree the dirty check
/// ignores, so logs anywhere else would deadlock the next run.
pub(crate) const JSONL_LOG_PATH: &str = ".harness/runs/harness.log.jsonl";
pub(crate) const NOTES_LOG_PATH: &str = ".harness/runs/harness-notes.md";

/// `specDefaults.ts:132-150`. Top-level keys the loader understands; anything else warns.
const KNOWN_KEYS: [&str; 17] = [
    "schemaVersion",
    "name",
    "description",
    "prd",
    "eval",
    "agent",
    "generator",
    "validator",
    "constraints",
    "templateMetadata",
    "validators",
    "requiredFiles",
    "forbiddenFiles",
    "secretScan",
    "chainValidation",
    "baseline",
    "maxAttempts",
];

/// `specDefaults.ts:153-158`. Hard-fail at load: a silent drop would burn a generator session
/// before EVALUATE noticed.
const REMOVED_KEYS: [(&str, &str); 4] = [
    ("contract", "use eval: not contract:"),
    ("extend", "use baseline: not extend:"),
    (
        "logging",
        "remove logging: — harness logs always live under .harness/runs/",
    ),
    (
        "skills",
        "remove skills: — product skills from hedera-skills are loaded automatically",
    ),
];

const PACKAGE_MANAGERS: [&str; 3] = ["yarn", "npm", "pnpm"];

/// `specDefaults.ts:114-119`.
const DEFAULT_SECRET_PATTERN_NAME: &str = "private-key-assignment";
const DEFAULT_SECRET_PATTERN: &str =
    r"(PRIVATE_KEY|OPERATOR_KEY|HEDERA_OPERATOR_PRIVATE_KEY)\s*=\s*(0x)?[0-9a-fA-F]{32,}";

/// Fork `specDefaults.ts:25-33`: hiero-local-node's published endpoints and env var names.
const DEFAULT_LOCAL_RPC_URL: &str = "http://localhost:7546";
const DEFAULT_LOCAL_GRPC_URL: &str = "localhost:50211";
const DEFAULT_LOCAL_MIRROR_URL: &str = "http://localhost:5551";
const DEFAULT_OPERATOR_ACCOUNT_ID_ENV: &str = "HEDERA_OPERATOR_ID";
const DEFAULT_OPERATOR_PRIVATE_KEY_ENV: &str = "HEDERA_OPERATOR_KEY";

/// Mirror transaction types Hanvil records (`state::hapi::BodyKind::name`). An assertion on any
/// other type could never pass, so it is refused at load.
pub(crate) const RECORDED_TRANSACTION_TYPES: [&str; 6] = [
    "CRYPTOCREATEACCOUNT",
    "CRYPTOTRANSFER",
    "CRYPTODELETE",
    "CONSENSUSCREATETOPIC",
    "CONSENSUSSUBMITMESSAGE",
    "ETHEREUMTRANSACTION",
];

/// What stops a recipe from loading.
#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    /// Hanvil: there is no file at the path, which is the first thing a new project hits.
    #[error("no recipe at {path}. Pass the recipe path, or bootstrap one with `hanvil init`.")]
    Missing {
        /// The recipe.
        path: PathBuf,
    },
    /// The file could not be read.
    #[error("reading {path}: {source}")]
    Read {
        /// The recipe.
        path: PathBuf,
        /// The OS error.
        #[source]
        source: std::io::Error,
    },
    /// The file is not YAML.
    #[error("{path}: {source}")]
    Yaml {
        /// The recipe.
        path: PathBuf,
        /// The parser's error.
        #[source]
        source: serde_yaml_ng::Error,
    },
    /// The recipe violates the schema. The message is the TypeScript loader's.
    #[error("{0}")]
    Invalid(String),
}

fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid(message.into())
}

/// `specDefaults.ts:27`. Which agent CLI family the run targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentPreset {
    /// Cursor's `agent` CLI.
    Cursor,
    /// Anthropic's `claude` CLI. The default.
    Claude,
}

/// `specDefaults.ts:30-32`. How the browser MCP server reaches the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum McpDelivery {
    /// A harness-owned file passed by flag.
    ConfigFlag(&'static str),
    /// A fixed path inside the workspace the CLI reads on its own.
    WorkspaceFile(&'static str),
}

impl AgentPreset {
    /// `specDefaults.ts:104`.
    pub(crate) const DEFAULT: Self = Self::Claude;

    fn parse(name: &str) -> Option<Self> {
        match name {
            "cursor" => Some(Self::Cursor),
            "claude" => Some(Self::Claude),
            _ => None,
        }
    }

    /// The recipe's spelling.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Cursor => "cursor",
            Self::Claude => "claude",
        }
    }

    /// The binary on PATH.
    pub(crate) fn command(self) -> &'static str {
        match self {
            Self::Cursor => "agent",
            Self::Claude => "claude",
        }
    }

    /// `specDefaults.ts:48-60` and `:73-83`. Generator argv before the model flag.
    fn args(self) -> &'static [&'static str] {
        match self {
            Self::Cursor => &[
                "-p",
                "--trust",
                "--sandbox",
                "enabled",
                "--workspace",
                "{workspace}",
                "--force",
                "--approve-mcps",
                "--output-format",
                "stream-json",
                "--stream-partial-output",
            ],
            Self::Claude => &[
                "-p",
                "{prompt}",
                "--permission-mode",
                "acceptEdits",
                "--allowedTools",
                "Bash,Read,Edit,Write",
                "--output-format",
                "stream-json",
                "--verbose",
            ],
        }
    }

    /// `specDefaults.ts:88-96`. Read-only validator argv; MCP tools must be named in
    /// `--allowedTools` because `acceptEdits` auto-accepts edits only.
    fn validator_args(self) -> &'static [&'static str] {
        match self {
            Self::Cursor => self.args(),
            Self::Claude => &[
                "-p",
                "{prompt}",
                "--allowedTools",
                "mcp__playwright,Read,Grep,Glob",
                "--output-format",
                "stream-json",
                "--verbose",
            ],
        }
    }

    /// `specDefaults.ts:61,84`.
    pub(crate) fn timeout_ms(self) -> u64 {
        3_600_000
    }

    /// `specDefaults.ts:66,98`.
    pub(crate) fn model_flag(self) -> &'static str {
        "--model"
    }

    /// `specDefaults.ts:67,99`.
    pub(crate) fn default_model(self) -> &'static str {
        match self {
            Self::Cursor => "composer-2.5",
            Self::Claude => "opus",
        }
    }

    /// `specDefaults.ts:68,100`.
    pub(crate) fn repair_model(self) -> &'static str {
        match self {
            Self::Cursor => "composer-2.5",
            Self::Claude => "sonnet",
        }
    }

    /// `specDefaults.ts:65,97`.
    pub(crate) fn mcp(self) -> McpDelivery {
        match self {
            Self::Cursor => McpDelivery::WorkspaceFile(".cursor/mcp.json"),
            Self::Claude => McpDelivery::ConfigFlag("--mcp-config"),
        }
    }

    /// `specLoader.ts:290-305`. Invocation for a role with the model flag applied.
    fn command_config(self, validator: bool) -> CommandConfig {
        let base = if validator {
            self.validator_args()
        } else {
            self.args()
        };
        let mut args: Vec<String> = base.iter().map(|arg| (*arg).to_string()).collect();
        args.push(self.model_flag().to_string());
        args.push(self.default_model().to_string());
        CommandConfig {
            command: self.command().to_string(),
            args: Some(args),
            env: None,
            timeout_ms: Some(self.timeout_ms()),
        }
    }
}

/// `types.ts` `CommandAgentConfig`: how to spawn an agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommandConfig {
    /// Binary or path.
    pub(crate) command: String,
    /// Argv; `{workspace}` and `{prompt}` are substituted, and the prompt is appended when no
    /// argument carries it.
    pub(crate) args: Option<Vec<String>>,
    /// Extra environment on top of the harness's own.
    pub(crate) env: Option<BTreeMap<String, String>>,
    /// Wall-clock limit.
    pub(crate) timeout_ms: Option<u64>,
}

/// `validator:` block after defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ValidatorConfig {
    /// How to spawn the validator agent.
    pub(crate) command: CommandConfig,
    /// `enabled: false` keeps a stub so `isValidatorEnabled` can say no.
    pub(crate) enabled: bool,
}

/// `constraints:` block after defaults.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Constraints {
    /// `yarn`, `npm`, `pnpm` or anything else.
    pub(crate) package_manager: Option<String>,
    /// Workspace directories; each also gets a default `<ws>/.env` secret file.
    pub(crate) workspaces: Option<Vec<String>>,
    /// Workspaces the agent must not touch.
    pub(crate) forbidden_workspaces: Option<Vec<String>>,
    /// Every other package manager's `install`/`run` unless the recipe lists its own.
    pub(crate) forbidden_commands: Vec<String>,
}

/// `templateMetadata:` block.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TemplateMetadata {
    /// Expected `template.json` name.
    pub(crate) name: Option<String>,
    /// Expected frontend capability.
    pub(crate) frontend: Option<String>,
    /// Expected Solidity framework capability.
    pub(crate) solidity_framework: Option<String>,
}

/// `validators:` block, paths resolved against the project root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Validators {
    /// File, JSON and text assertions.
    pub(crate) static_path: PathBuf,
    /// Commands that must exit 0.
    pub(crate) commands_path: PathBuf,
    /// SMOKE gate config; absent means SMOKE is off.
    pub(crate) playwright_path: Option<PathBuf>,
}

/// One `secretScan.patterns[]` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SecretPattern {
    /// Appears in the finding id.
    pub(crate) name: String,
    /// A JavaScript regular expression, compiled with the `m` flag.
    pub(crate) pattern: String,
    /// Relative paths where a match is fine.
    pub(crate) allow_in: Vec<String>,
}

/// `secretScan:` block after defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SecretScan {
    /// Paths whose existence is a finding.
    pub(crate) fail_on_files: Vec<String>,
    /// Patterns scanned over source files.
    pub(crate) patterns: Vec<SecretPattern>,
}

/// One command of `baseline.commands` or `chainValidation.deploy.commands`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommandSpec {
    /// Optional for baseline, required for deploy. `install` is special-cased.
    pub(crate) name: Option<String>,
    /// Run through the shell in the workspace.
    pub(crate) command: String,
    /// Per-command limit.
    pub(crate) timeout_ms: Option<u64>,
}

/// Fork `types.ts` `ChainNetwork`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChainNetwork {
    /// Hedera testnet. Loaded for parity; `hanvil run` refuses it.
    Testnet,
    /// A hiero-local-node-compatible node — Hanvil itself.
    Local,
}

impl ChainNetwork {
    /// The recipe's spelling.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Testnet => "testnet",
            Self::Local => "local",
        }
    }
}

/// Fork `types.ts` `ChainLocalConfig`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChainLocal {
    /// JSON-RPC.
    pub(crate) rpc_url: String,
    /// HAPI gRPC, `host:port`.
    pub(crate) grpc_url: String,
    /// Mirror REST.
    pub(crate) mirror_url: String,
}

/// An account named in a chain assertion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AccountRef {
    /// The signer this run provisioned.
    Signer,
    /// `0.0.N`.
    Id(String),
    /// A 20-byte EVM address.
    Evm(String),
}

/// A topic named in a chain assertion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TopicRef {
    /// The newest topic on the chain — the one the attempt's deploy or app created last.
    Created,
    /// `0.0.N`.
    Id(String),
}

/// One `chainValidation.assert[]` entry. Each is evaluated on the chain after the deploy
/// commands and produces at most one finding.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ChainAssertion {
    /// Balance, existence or deletion of an account.
    Account {
        /// Which account.
        account: AccountRef,
        /// Balance must be at least this many HBAR.
        min_balance_hbar: Option<f64>,
        /// The account must (not) exist.
        exists: Option<bool>,
        /// The account must (not) be deleted.
        deleted: Option<bool>,
    },
    /// Code at an EVM address.
    Contract {
        /// The address.
        address: String,
        /// Code must (not) be present.
        deployed: bool,
    },
    /// Message count of a topic.
    Topic {
        /// Which topic.
        topic: TopicRef,
        /// Sequence number reached.
        messages_at_least: u64,
    },
    /// Transactions recorded since the attempt's snapshot.
    Transactions {
        /// A mirror transaction type from `RECORDED_TRANSACTION_TYPES`.
        kind: String,
        /// Only count those paid by this account.
        payer: Option<AccountRef>,
        /// Minimum count.
        at_least: u64,
    },
    /// Submissions the node refused before consensus, since the attempt's snapshot. These leave
    /// no record, no receipt and no mirror row, so an app that catches the error and carries on
    /// passes every other gate; this is the one that fails.
    Rejections {
        /// Only count this transaction type; any type when absent, including the `UNKNOWN` of a
        /// body that never decoded.
        kind: Option<String>,
        /// Only count those the payer would have paid for.
        payer: Option<AccountRef>,
        /// Maximum tolerated. Zero means the app must have none.
        at_most: u64,
    },
}

/// `chainValidation:` block after defaults. Absent, or `enabled: false`, means CHAIN is off.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ChainValidation {
    /// Where the signer lives.
    pub(crate) network: ChainNetwork,
    /// Env var holding the operator's account id.
    pub(crate) operator_account_id_env: String,
    /// Env var holding the operator's private key.
    pub(crate) operator_private_key_env: String,
    /// Endpoints; set only on `local`.
    pub(crate) local: Option<ChainLocal>,
    /// Signer funding.
    pub(crate) funding_hbar: f64,
    /// Sweep the signer back to the operator at run end.
    pub(crate) sweep_back: bool,
    /// localStorage key the burner connector reads.
    pub(crate) browser_local_storage_key: String,
    /// Env var names that receive the signer's private key.
    pub(crate) expose_env_vars: Vec<String>,
    /// Run every attempt after GENERATE, before SMOKE.
    pub(crate) deploy: Vec<CommandSpec>,
    /// Hanvil: snapshot before GENERATE, revert after a failed attempt. Default on.
    pub(crate) snapshot_per_attempt: bool,
    /// Hanvil: seconds of chain time to advance before the assertions.
    pub(crate) advance_time_seconds: u64,
    /// Hanvil: deterministic checks on the chain.
    pub(crate) assertions: Vec<ChainAssertion>,
}

/// A loaded recipe. Paths are absolute.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Spec {
    /// Always 3.
    pub(crate) schema_version: u64,
    /// The recipe file.
    pub(crate) spec_path: PathBuf,
    /// Parent of the recipe's directory: `.harness/spec.yaml` describes `.`.
    pub(crate) project_root: PathBuf,
    /// Used in branch names and run ids.
    pub(crate) name: String,
    /// Free text.
    pub(crate) description: Option<String>,
    /// One per increment, in delivery order.
    pub(crate) prd_paths: Vec<PathBuf>,
    /// Absent means EVALUATE is not configured; one path grades every slice, a list is 1:1.
    pub(crate) eval_paths: Option<Vec<PathBuf>>,
    /// Governs MCP delivery and models even when `generator:` overrides the invocation.
    pub(crate) agent: AgentPreset,
    /// How to spawn the generator.
    pub(crate) generator: CommandConfig,
    /// How to spawn the validator, if EVALUATE is configured at all.
    pub(crate) validator: Option<ValidatorConfig>,
    /// Package manager and workspaces.
    pub(crate) constraints: Constraints,
    /// Expected template capabilities.
    pub(crate) template_metadata: Option<TemplateMetadata>,
    /// Validator config paths.
    pub(crate) validators: Validators,
    /// Files that must exist after GENERATE.
    pub(crate) required_files: Vec<String>,
    /// Files that must not.
    pub(crate) forbidden_files: Vec<String>,
    /// Secret scan config.
    pub(crate) secret_scan: SecretScan,
    /// CHAIN config.
    pub(crate) chain_validation: Option<ChainValidation>,
    /// Host-app health commands, run once before generation. Never empty; one is named
    /// `install`.
    pub(crate) baseline: Vec<CommandSpec>,
    /// Attempt budget per slice.
    pub(crate) max_attempts: u64,
    /// `.harness/runs/harness.log.jsonl`, absolute.
    pub(crate) jsonl_log_path: PathBuf,
    /// `.harness/runs/harness-notes.md`, absolute.
    pub(crate) notes_log_path: PathBuf,
}

impl Spec {
    /// `evaluation.ts:19-21`.
    pub(crate) fn validator_enabled(&self) -> bool {
        self.validator.as_ref().is_some_and(|v| v.enabled)
    }

    /// `sliceSelection.ts:28-45`: the PRD and eval pair for an increment. A scalar `eval`
    /// grades every increment; a list is 1:1. An index past the end clamps to the last.
    pub(crate) fn slice(&self, index: usize) -> (PathBuf, Option<PathBuf>) {
        let index = index.min(self.prd_paths.len().saturating_sub(1));
        let prd = self.prd_paths[index].clone();
        let eval = self.eval_paths.as_ref().and_then(|evals| {
            if evals.len() == 1 {
                evals.first().cloned()
            } else {
                evals.get(index).cloned()
            }
        });
        (prd, eval)
    }
}

/// A recipe plus what the loader wanted to say about it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Loaded {
    /// The recipe.
    pub(crate) spec: Spec,
    /// Unknown keys and the like. Not yet printed; the caller decides the prefix.
    pub(crate) warnings: Vec<String>,
}

/// Read and validate a recipe file.
pub(crate) fn load(spec_path: &Path) -> Result<Loaded, Error> {
    let raw = std::fs::read_to_string(spec_path).map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            Error::Missing {
                path: spec_path.to_path_buf(),
            }
        } else {
            Error::Read {
                path: spec_path.to_path_buf(),
                source,
            }
        }
    })?;
    parse(&raw, spec_path)
}

/// Validate recipe text as if it lived at `spec_path`. `specLoader.ts:31-90`.
pub(crate) fn parse(raw: &str, spec_path: &Path) -> Result<Loaded, Error> {
    let absolute = std::path::absolute(spec_path).map_err(|source| Error::Read {
        path: spec_path.to_path_buf(),
        source,
    })?;
    let value: Value = serde_yaml_ng::from_str(raw).map_err(|source| Error::Yaml {
        path: absolute.clone(),
        source,
    })?;
    let empty = Map::new();
    let parsed = match &value {
        Value::Object(map) => map,
        // `parseYaml(raw) ?? {}`: an empty file is an empty recipe.
        Value::Null => &empty,
        other => {
            return Err(invalid(format!(
                "{} is not a mapping (got {}).",
                absolute.display(),
                serde_json::to_string(other).unwrap_or_default()
            )));
        }
    };
    let spec_display = absolute.display().to_string();
    // The project root is the parent of the spec's directory (`specLoader.ts:35-37`).
    let project_root = absolute
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("/"));
    let mut warnings = Vec::new();

    let schema_version = read_schema_version(parsed, &spec_display)?;
    reject_removed_keys(parsed, &spec_display)?;
    warn_unknown_keys(parsed, &mut warnings);

    let constraints = read_constraints(parsed)?;
    let workspaces = constraints
        .as_ref()
        .and_then(|c| c.workspaces.clone())
        .unwrap_or_default();
    let agent = read_agent_preset(parsed)?;
    let prd_paths = read_prd_paths(parsed, &project_root)?;
    let eval_paths = read_eval_paths(parsed, &project_root, prd_paths.len())?;

    let forbidden_commands = constraints
        .as_ref()
        .and_then(|c| c.forbidden_commands_override.clone())
        .unwrap_or_else(|| {
            default_forbidden_commands(
                constraints
                    .as_ref()
                    .and_then(|c| c.package_manager.as_deref()),
            )
        });
    let constraints = Constraints {
        package_manager: constraints.as_ref().and_then(|c| c.package_manager.clone()),
        workspaces: constraints.as_ref().and_then(|c| c.workspaces.clone()),
        forbidden_workspaces: constraints
            .as_ref()
            .and_then(|c| c.forbidden_workspaces.clone()),
        forbidden_commands,
    };

    let baseline = read_baseline(parsed)?;
    assert_baseline_has_install(baseline.as_deref())?;
    let baseline = baseline.unwrap_or_default();

    let spec = Spec {
        schema_version,
        spec_path: absolute,
        project_root: project_root.clone(),
        name: read_string(parsed, "name")?,
        description: read_optional_string(parsed, "description"),
        prd_paths,
        eval_paths,
        agent,
        generator: read_generator(parsed, agent)?,
        validator: read_optional_validator(parsed, agent)?,
        constraints,
        template_metadata: read_template_metadata(parsed),
        validators: read_validators(parsed, &project_root)?,
        required_files: read_optional_string_array(parsed, "requiredFiles")?.unwrap_or_default(),
        forbidden_files: read_optional_string_array(parsed, "forbiddenFiles")?
            .unwrap_or_else(|| default_secret_files(&workspaces)),
        secret_scan: read_secret_scan(parsed, &workspaces)?,
        chain_validation: read_chain_validation(parsed)?,
        baseline,
        max_attempts: read_optional_number(parsed, "maxAttempts")
            .map_or(DEFAULT_MAX_ATTEMPTS, |n| n as u64),
        jsonl_log_path: resolve_project_path(&project_root, JSONL_LOG_PATH),
        notes_log_path: resolve_project_path(&project_root, NOTES_LOG_PATH),
    };

    Ok(Loaded { spec, warnings })
}

/// `specLoader.ts:100-134`.
fn read_schema_version(parsed: &Map<String, Value>, spec_path: &str) -> Result<u64, Error> {
    let Some(raw) = parsed.get("schemaVersion") else {
        return Err(invalid(format!(
            "{spec_path} is missing schemaVersion. Set schemaVersion: {SCHEMA_VERSION}."
        )));
    };
    let version = match raw.as_f64() {
        Some(n) if n.fract() == 0.0 && n >= 1.0 => n as u64,
        _ => {
            return Err(invalid(format!(
                "\"schemaVersion\" must be a positive integer in {spec_path} (got {}).",
                serde_json::to_string(raw).unwrap_or_default()
            )));
        }
    };
    if version > SCHEMA_VERSION {
        return Err(invalid(format!(
            "{spec_path} declares schemaVersion {version}, but this harness understands up to \
             {SCHEMA_VERSION}. Upgrade the harness: npm install hedera-harness@latest"
        )));
    }
    if version < SCHEMA_VERSION {
        return Err(invalid(format!(
            "{spec_path} declares schemaVersion {version}, which this harness no longer supports. \
             Set schemaVersion: {SCHEMA_VERSION}."
        )));
    }
    Ok(version)
}

/// `specLoader.ts:141-151`.
fn reject_removed_keys(parsed: &Map<String, Value>, spec_path: &str) -> Result<(), Error> {
    let removed: Vec<(&str, &str)> = REMOVED_KEYS
        .iter()
        .copied()
        .filter(|(key, _)| parsed.contains_key(*key))
        .collect();
    if removed.is_empty() {
        return Ok(());
    }
    let names: Vec<&str> = removed.iter().map(|(key, _)| *key).collect();
    let mut message = format!("{spec_path} uses removed key(s): {}.", names.join(", "));
    for (_, hint) in removed {
        message.push(' ');
        message.push_str(hint);
    }
    Err(invalid(message))
}

/// `specLoader.ts:157-167`. Top-level only; nested unknown keys are never reported.
fn warn_unknown_keys(parsed: &Map<String, Value>, warnings: &mut Vec<String>) {
    let unknown: Vec<&str> = parsed
        .keys()
        .map(String::as_str)
        .filter(|key| !KNOWN_KEYS.contains(key) && !REMOVED_KEYS.iter().any(|(k, _)| k == key))
        .collect();
    if !unknown.is_empty() {
        warnings.push(format!(
            "ignoring unknown key(s): {}. If these come from a newer recipe, upgrade the harness.",
            unknown.join(", ")
        ));
    }
}

/// `specLoader.ts:173-195`.
fn read_prd_paths(parsed: &Map<String, Value>, project_root: &Path) -> Result<Vec<PathBuf>, Error> {
    match parsed.get("prd") {
        // No warning: the generated skeleton omits `prd` on purpose.
        None => Ok(vec![resolve_project_path(project_root, DEFAULT_PRD_PATH)]),
        Some(Value::String(raw)) => {
            if raw.trim().is_empty() {
                return Err(invalid(
                    "Expected non-empty string \"prd\" in template spec.",
                ));
            }
            Ok(vec![resolve_project_path(project_root, raw)])
        }
        Some(raw) => {
            let list = non_empty_string_list(raw).ok_or_else(|| {
                invalid("Expected \"prd\" to be a path or a non-empty list of paths.")
            })?;
            if list.is_empty() {
                return Err(invalid("Expected \"prd\" to list at least one PRD path."));
            }
            Ok(list
                .iter()
                .map(|value| resolve_project_path(project_root, value))
                .collect())
        }
    }
}

/// `specLoader.ts:201-229`.
fn read_eval_paths(
    parsed: &Map<String, Value>,
    project_root: &Path,
    prd_count: usize,
) -> Result<Option<Vec<PathBuf>>, Error> {
    match parsed.get("eval") {
        None => Ok(None),
        Some(Value::String(raw)) => {
            if raw.trim().is_empty() {
                return Err(invalid(
                    "Expected non-empty string \"eval\" in template spec.",
                ));
            }
            Ok(Some(vec![resolve_project_path(project_root, raw)]))
        }
        Some(raw) => {
            let list = non_empty_string_list(raw).ok_or_else(|| {
                invalid("Expected \"eval\" to be a path or a non-empty list of paths.")
            })?;
            if list.is_empty() {
                return Err(invalid("Expected \"eval\" to list at least one path."));
            }
            if list.len() != prd_count {
                return Err(invalid(format!(
                    "\"eval\" list has {} path(s) but \"prd\" has {prd_count}; list form must be 1:1.",
                    list.len()
                )));
            }
            Ok(Some(
                list.iter()
                    .map(|value| resolve_project_path(project_root, value))
                    .collect(),
            ))
        }
    }
}

/// An array whose every item is a non-blank string, else `None`.
fn non_empty_string_list(value: &Value) -> Option<Vec<&str>> {
    let items = value.as_array()?;
    items
        .iter()
        .map(|item| item.as_str().filter(|s| !s.trim().is_empty()))
        .collect()
}

/// `specLoader.ts:231-251`.
fn read_validators(parsed: &Map<String, Value>, project_root: &Path) -> Result<Validators, Error> {
    let empty = Map::new();
    let validators = as_object(parsed.get("validators")).unwrap_or(&empty);
    let playwright_path = match validators.get("playwright") {
        None => None,
        Some(candidate) => match candidate.as_str().filter(|s| !s.trim().is_empty()) {
            Some(path) => Some(resolve_project_path(project_root, path)),
            None => {
                return Err(invalid(
                    "Expected optional non-empty string \"validators.playwright\" in template spec.",
                ));
            }
        },
    };
    Ok(Validators {
        static_path: resolve_project_path(
            project_root,
            &read_optional_string(validators, "static")
                .unwrap_or_else(|| DEFAULT_STATIC_VALIDATOR_PATH.to_string()),
        ),
        commands_path: resolve_project_path(
            project_root,
            &read_optional_string(validators, "commands")
                .unwrap_or_else(|| DEFAULT_COMMANDS_VALIDATOR_PATH.to_string()),
        ),
        playwright_path,
    })
}

/// `specLoader.ts:253-255`.
fn resolve_project_path(project_root: &Path, value: &str) -> PathBuf {
    let path = Path::new(value);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        project_root.join(path)
    }
}

/// `specLoader.ts:276-287`.
fn read_agent_preset(parsed: &Map<String, Value>) -> Result<AgentPreset, Error> {
    let requested = read_optional_string(parsed, "agent")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let Some(requested) = requested else {
        return Ok(AgentPreset::DEFAULT);
    };
    AgentPreset::parse(&requested).ok_or_else(|| {
        invalid(format!(
            "Unknown agent preset {}. Available: cursor, claude.",
            serde_json::to_string(&requested).unwrap_or_default()
        ))
    })
}

/// `specLoader.ts:313-326`.
fn read_generator(parsed: &Map<String, Value>, agent: AgentPreset) -> Result<CommandConfig, Error> {
    if !parsed.contains_key("generator") {
        return Ok(agent.command_config(false));
    }
    let generator = read_object(parsed, "generator")?;
    Ok(CommandConfig {
        command: read_string(generator, "command")?,
        args: read_optional_string_array(generator, "args")?,
        env: read_optional_string_record(generator, "env")?,
        timeout_ms: read_optional_number(generator, "timeoutMs").map(|n| n as u64),
    })
}

/// `specLoader.ts:333-361`.
fn read_optional_validator(
    parsed: &Map<String, Value>,
    agent: AgentPreset,
) -> Result<Option<ValidatorConfig>, Error> {
    let Some(validator) = parsed.get("validator") else {
        return Ok(None);
    };
    let Some(record) = as_object(Some(validator)) else {
        return Err(invalid("Expected object \"validator\" in template spec."));
    };
    if record.get("enabled") == Some(&Value::Bool(false)) {
        return Ok(Some(ValidatorConfig {
            command: CommandConfig {
                command: read_optional_string(record, "command")
                    .unwrap_or_else(|| agent.command().to_string()),
                args: None,
                env: None,
                timeout_ms: None,
            },
            enabled: false,
        }));
    }
    if !record.contains_key("command") {
        return Ok(Some(ValidatorConfig {
            command: agent.command_config(true),
            enabled: true,
        }));
    }
    Ok(Some(ValidatorConfig {
        command: CommandConfig {
            command: read_string(record, "command")?,
            args: read_optional_string_array(record, "args")?,
            env: read_optional_string_record(record, "env")?,
            timeout_ms: read_optional_number(record, "timeoutMs").map(|n| n as u64),
        },
        enabled: true,
    }))
}

/// `constraints:` as read, before the forbidden-commands default is applied.
struct RawConstraints {
    package_manager: Option<String>,
    workspaces: Option<Vec<String>>,
    forbidden_workspaces: Option<Vec<String>>,
    forbidden_commands_override: Option<Vec<String>>,
}

/// `specLoader.ts:424-436`.
fn read_constraints(parsed: &Map<String, Value>) -> Result<Option<RawConstraints>, Error> {
    let Some(record) = as_object(parsed.get("constraints")) else {
        return Ok(None);
    };
    Ok(Some(RawConstraints {
        package_manager: read_optional_string(record, "packageManager"),
        workspaces: read_optional_string_array(record, "workspaces")?,
        forbidden_workspaces: read_optional_string_array(record, "forbiddenWorkspaces")?,
        forbidden_commands_override: read_optional_string_array(record, "forbiddenCommands")?,
    }))
}

/// `specDefaults.ts:124-130`. Every package manager except the project's.
fn default_forbidden_commands(package_manager: Option<&str>) -> Vec<String> {
    let requested = package_manager.map(|pm| pm.trim().to_lowercase());
    let active = PACKAGE_MANAGERS
        .iter()
        .find(|name| requested.as_deref().is_some_and(|pm| pm.starts_with(*name)));
    PACKAGE_MANAGERS
        .iter()
        .filter(|name| Some(name) != active.as_ref())
        .flat_map(|name| [format!("{name} install"), format!("{name} run")])
        .collect()
}

/// `specDefaults.ts:110-112`.
fn default_secret_files(workspaces: &[String]) -> Vec<String> {
    std::iter::once(".env".to_string())
        .chain(workspaces.iter().map(|ws| format!("{ws}/.env")))
        .collect()
}

/// `specLoader.ts:438-449`.
fn read_template_metadata(parsed: &Map<String, Value>) -> Option<TemplateMetadata> {
    let record = as_object(parsed.get("templateMetadata"))?;
    Some(TemplateMetadata {
        name: read_optional_string(record, "name"),
        frontend: read_optional_string(record, "frontend"),
        solidity_framework: read_optional_string(record, "solidityFramework"),
    })
}

/// `specLoader.ts:455-477`. Non-array values fall back to the defaults silently, as upstream.
/// A pattern entry without `name` and `pattern` is refused here rather than crashing the scan
/// later, which is where upstream would fail.
fn read_secret_scan(
    parsed: &Map<String, Value>,
    workspaces: &[String],
) -> Result<SecretScan, Error> {
    let default_patterns = || {
        vec![SecretPattern {
            name: DEFAULT_SECRET_PATTERN_NAME.to_string(),
            pattern: DEFAULT_SECRET_PATTERN.to_string(),
            allow_in: Vec::new(),
        }]
    };
    let Some(record) = as_object(parsed.get("secretScan")) else {
        return Ok(SecretScan {
            fail_on_files: default_secret_files(workspaces),
            patterns: default_patterns(),
        });
    };
    let fail_on_files = match record.get("failOnFiles").and_then(Value::as_array) {
        Some(items) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        None => default_secret_files(workspaces),
    };
    let patterns = match record.get("patterns").and_then(Value::as_array) {
        Some(items) => items
            .iter()
            .map(|item| {
                let entry = as_object(Some(item)).ok_or_else(|| {
                    invalid("Expected {name, pattern} objects in \"secretScan.patterns\".")
                })?;
                Ok(SecretPattern {
                    name: read_string(entry, "name")?,
                    pattern: read_string(entry, "pattern")?,
                    allow_in: read_optional_string_array(entry, "allowIn")?.unwrap_or_default(),
                })
            })
            .collect::<Result<Vec<_>, Error>>()?,
        None => default_patterns(),
    };
    Ok(SecretScan {
        fail_on_files,
        patterns,
    })
}

/// `specLoader.ts:484-514`. `None` when the key is absent; `Some(vec![])` when `baseline: {}`.
fn read_baseline(parsed: &Map<String, Value>) -> Result<Option<Vec<CommandSpec>>, Error> {
    let Some(raw) = parsed.get("baseline") else {
        return Ok(None);
    };
    let Some(record) = as_object(Some(raw)) else {
        return Err(invalid("Expected object \"baseline\" in template spec."));
    };
    let Some(commands) = record.get("commands") else {
        return Ok(Some(Vec::new()));
    };
    let Some(items) = commands.as_array() else {
        return Err(invalid(
            "Expected array \"baseline.commands\" in template spec.",
        ));
    };
    items
        .iter()
        .enumerate()
        .map(|(index, item)| match item {
            Value::String(command) => Ok(CommandSpec {
                name: None,
                command: command.clone(),
                timeout_ms: None,
            }),
            other => {
                let Some(cmd) = as_object(Some(other)) else {
                    return Err(invalid(format!(
                        "Expected string or object at baseline.commands[{index}]."
                    )));
                };
                Ok(CommandSpec {
                    name: read_optional_string(cmd, "name"),
                    command: read_string(cmd, "command")?,
                    timeout_ms: read_optional_number(cmd, "timeoutMs").map(|n| n as u64),
                })
            }
        })
        .collect::<Result<Vec<_>, Error>>()
        .map(Some)
}

/// `specLoader.ts:518-538`. Enforced for every command, `validate` included, as upstream.
fn assert_baseline_has_install(commands: Option<&[CommandSpec]>) -> Result<(), Error> {
    let Some(commands) = commands.filter(|c| !c.is_empty()) else {
        return Err(invalid(
            "run requires baseline.commands including a command literally named \"install\" \
             (used for host-health checks and install fingerprinting).",
        ));
    };
    if !commands
        .iter()
        .any(|command| command.name.as_deref() == Some("install"))
    {
        return Err(invalid(
            "baseline.commands must include a command literally named \"install\" \
             (used for host-health checks and install fingerprinting).",
        ));
    }
    Ok(())
}

/// Fork `specLoader.ts` `readChainValidation`, plus Hanvil's three keys.
fn read_chain_validation(parsed: &Map<String, Value>) -> Result<Option<ChainValidation>, Error> {
    let Some(raw) = parsed.get("chainValidation") else {
        return Ok(None);
    };
    let Some(record) = as_object(Some(raw)) else {
        return Err(invalid(
            "Expected object \"chainValidation\" in template spec.",
        ));
    };
    if record.get("enabled") == Some(&Value::Bool(false)) {
        return Ok(None);
    }

    let network_name = read_string(record, "network")?;
    let network = match network_name.as_str() {
        "testnet" => ChainNetwork::Testnet,
        "local" => ChainNetwork::Local,
        _ => {
            return Err(invalid(format!(
                "chainValidation.network must be \"testnet\" or \"local\" (got {}). Mainnet is not allowed.",
                serde_json::to_string(&network_name).unwrap_or_default()
            )));
        }
    };

    // On local the operator falls back to the node's predefined account, so a recipe that
    // names no operator is complete.
    let empty = Map::new();
    let operator = if network == ChainNetwork::Local && !record.contains_key("operator") {
        &empty
    } else {
        read_object(record, "operator")?
    };
    let local_record = as_object(record.get("local"));
    if local_record.is_some() && network != ChainNetwork::Local {
        return Err(invalid(
            "chainValidation.local is only valid with network: \"local\".",
        ));
    }
    let expose = as_object(record.get("expose")).unwrap_or(&empty);

    let deploy = match as_object(record.get("deploy")) {
        None => Vec::new(),
        Some(deploy_record) => {
            let Some(items) = deploy_record.get("commands").and_then(Value::as_array) else {
                return Err(invalid(
                    "Expected array \"chainValidation.deploy.commands\" in template spec.",
                ));
            };
            items
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    let Some(cmd) = as_object(Some(item)) else {
                        return Err(invalid(format!(
                            "Expected object at chainValidation.deploy.commands[{index}]."
                        )));
                    };
                    Ok(CommandSpec {
                        name: Some(read_string(cmd, "name")?),
                        command: read_string(cmd, "command")?,
                        timeout_ms: read_optional_number(cmd, "timeoutMs").map(|n| n as u64),
                    })
                })
                .collect::<Result<Vec<_>, Error>>()?
        }
    };

    let funding_hbar = read_optional_number(record, "fundingHbar").unwrap_or(10.0);
    if !funding_hbar.is_finite() || funding_hbar <= 0.0 {
        return Err(invalid(
            "Expected positive number \"chainValidation.fundingHbar\".",
        ));
    }

    // On testnet an operator is mandatory; on local a missing name falls back to the
    // documented one.
    let operator_env = |key: &str, fallback: &str| -> Result<String, Error> {
        if network == ChainNetwork::Local {
            Ok(read_optional_string(operator, key)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| fallback.to_string()))
        } else {
            read_string(operator, key)
        }
    };
    let local_url = |key: &str, fallback: &str| -> String {
        local_record
            .and_then(|r| read_optional_string(r, key))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| fallback.to_string())
    };

    let browser_local_storage_key = expose
        .get("browserLocalStorageKey")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("burnerWallet.pk")
        .to_string();

    Ok(Some(ChainValidation {
        network,
        operator_account_id_env: operator_env("accountIdEnv", DEFAULT_OPERATOR_ACCOUNT_ID_ENV)?,
        operator_private_key_env: operator_env("privateKeyEnv", DEFAULT_OPERATOR_PRIVATE_KEY_ENV)?,
        local: (network == ChainNetwork::Local).then(|| ChainLocal {
            rpc_url: local_url("rpcUrl", DEFAULT_LOCAL_RPC_URL),
            grpc_url: local_url("grpcUrl", DEFAULT_LOCAL_GRPC_URL),
            mirror_url: local_url("mirrorUrl", DEFAULT_LOCAL_MIRROR_URL),
        }),
        funding_hbar,
        sweep_back: record.get("sweepBack") != Some(&Value::Bool(false)),
        browser_local_storage_key,
        expose_env_vars: read_optional_string_array(expose, "envVars")?.unwrap_or_default(),
        deploy,
        snapshot_per_attempt: record.get("snapshotPerAttempt") != Some(&Value::Bool(false)),
        advance_time_seconds: read_advance_time(record)?,
        assertions: read_chain_assertions(record)?,
    }))
}

/// Hanvil: `chainValidation.advanceTimeSeconds`, a non-negative integer, default 0.
fn read_advance_time(record: &Map<String, Value>) -> Result<u64, Error> {
    match record.get("advanceTimeSeconds") {
        None => Ok(0),
        Some(raw) => match raw.as_f64() {
            Some(n) if n.fract() == 0.0 && n >= 0.0 => Ok(n as u64),
            _ => Err(invalid(
                "Expected non-negative integer \"chainValidation.advanceTimeSeconds\".",
            )),
        },
    }
}

/// Hanvil: `chainValidation.assert[]`.
fn read_chain_assertions(record: &Map<String, Value>) -> Result<Vec<ChainAssertion>, Error> {
    let Some(raw) = record.get("assert") else {
        return Ok(Vec::new());
    };
    let Some(items) = raw.as_array() else {
        return Err(invalid(
            "Expected array \"chainValidation.assert\" in template spec.",
        ));
    };
    items
        .iter()
        .enumerate()
        .map(|(index, item)| read_chain_assertion(index, item))
        .collect()
}

fn read_chain_assertion(index: usize, item: &Value) -> Result<ChainAssertion, Error> {
    let at = format!("chainValidation.assert[{index}]");
    let Some(entry) = as_object(Some(item)) else {
        return Err(invalid(format!("Expected object at {at}.")));
    };
    let kinds: Vec<&str> = ["account", "contract", "topic", "transactions", "rejections"]
        .into_iter()
        .filter(|key| entry.contains_key(*key))
        .collect();
    let [kind] = kinds.as_slice() else {
        return Err(invalid(format!(
            "{at} must have exactly one of: account, contract, topic, transactions, rejections."
        )));
    };
    match *kind {
        "account" => {
            let account = read_account_ref(entry, "account", &at)?;
            let min_balance_hbar = read_optional_number(entry, "minBalanceHbar");
            if min_balance_hbar.is_some_and(|n| !n.is_finite() || n < 0.0) {
                return Err(invalid(format!(
                    "Expected non-negative number \"{at}.minBalanceHbar\"."
                )));
            }
            let exists = read_optional_bool(entry, "exists", &at)?;
            let deleted = read_optional_bool(entry, "deleted", &at)?;
            if min_balance_hbar.is_none() && exists.is_none() && deleted.is_none() {
                return Err(invalid(format!(
                    "{at}.account needs minBalanceHbar, exists or deleted."
                )));
            }
            Ok(ChainAssertion::Account {
                account,
                min_balance_hbar,
                exists,
                deleted,
            })
        }
        "contract" => {
            let address = read_string(entry, "contract")?;
            if !is_evm_address(&address) {
                return Err(invalid(format!(
                    "{at}.contract must be a 0x-prefixed 20-byte address."
                )));
            }
            Ok(ChainAssertion::Contract {
                address,
                deployed: read_optional_bool(entry, "deployed", &at)?.unwrap_or(true),
            })
        }
        "topic" => {
            let topic = match read_string(entry, "topic")?.as_str() {
                "created" => TopicRef::Created,
                id if is_entity_id(id) => TopicRef::Id(id.to_string()),
                _ => {
                    return Err(invalid(format!(
                        "{at}.topic must be \"created\" or an id like 0.0.N."
                    )));
                }
            };
            let messages_at_least = match entry.get("messagesAtLeast").and_then(Value::as_f64) {
                Some(n) if n.fract() == 0.0 && n >= 0.0 => n as u64,
                _ => {
                    return Err(invalid(format!(
                        "Expected non-negative integer \"{at}.messagesAtLeast\"."
                    )));
                }
            };
            Ok(ChainAssertion::Topic {
                topic,
                messages_at_least,
            })
        }
        "transactions" => {
            let transactions = read_object(entry, "transactions")
                .map_err(|_| invalid(format!("Expected object \"{at}.transactions\".")))?;
            let kind = read_string(transactions, "type")?;
            if !RECORDED_TRANSACTION_TYPES.contains(&kind.as_str()) {
                return Err(invalid(format!(
                    "{at}.transactions.type must be one of: {}.",
                    RECORDED_TRANSACTION_TYPES.join(", ")
                )));
            }
            let payer = if transactions.contains_key("payer") {
                Some(read_account_ref(transactions, "payer", &at)?)
            } else {
                None
            };
            let at_least = match transactions.get("atLeast") {
                None => 1,
                Some(raw) => match raw.as_f64() {
                    Some(n) if n.fract() == 0.0 && n >= 1.0 => n as u64,
                    _ => {
                        return Err(invalid(format!(
                            "Expected positive integer \"{at}.transactions.atLeast\"."
                        )));
                    }
                },
            };
            Ok(ChainAssertion::Transactions {
                kind,
                payer,
                at_least,
            })
        }
        _ => {
            let rejections = read_object(entry, "rejections")
                .map_err(|_| invalid(format!("Expected object \"{at}.rejections\".")))?;
            let kind = if rejections.contains_key("type") {
                let kind = read_string(rejections, "type")?;
                if !RECORDED_TRANSACTION_TYPES.contains(&kind.as_str()) {
                    return Err(invalid(format!(
                        "{at}.rejections.type must be one of: {}.",
                        RECORDED_TRANSACTION_TYPES.join(", ")
                    )));
                }
                Some(kind)
            } else {
                None
            };
            let payer = if rejections.contains_key("payer") {
                Some(read_account_ref(rejections, "payer", &at)?)
            } else {
                None
            };
            let at_most = match rejections.get("atMost") {
                None => 0,
                Some(raw) => match raw.as_f64() {
                    Some(n) if n.fract() == 0.0 && n >= 0.0 => n as u64,
                    _ => {
                        return Err(invalid(format!(
                            "Expected non-negative integer \"{at}.rejections.atMost\"."
                        )));
                    }
                },
            };
            Ok(ChainAssertion::Rejections {
                kind,
                payer,
                at_most,
            })
        }
    }
}

fn read_account_ref(entry: &Map<String, Value>, key: &str, at: &str) -> Result<AccountRef, Error> {
    let raw = read_string(entry, key)?;
    match raw.as_str() {
        "signer" => Ok(AccountRef::Signer),
        id if is_entity_id(id) => Ok(AccountRef::Id(id.to_string())),
        address if is_evm_address(address) => Ok(AccountRef::Evm(address.to_string())),
        _ => Err(invalid(format!(
            "{at}.{key} must be \"signer\", an id like 0.0.N, or a 0x address."
        ))),
    }
}

fn is_entity_id(value: &str) -> bool {
    let mut parts = value.split('.');
    let all_digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    matches!(
        (parts.next(), parts.next(), parts.next(), parts.next()),
        (Some(a), Some(b), Some(c), None) if all_digits(a) && all_digits(b) && all_digits(c)
    )
}

fn is_evm_address(value: &str) -> bool {
    value
        .strip_prefix("0x")
        .is_some_and(|hex| hex.len() == 40 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// `readObject`: an object, and not an array. `specLoader.ts:363-369`.
fn read_object<'a>(
    value: &'a Map<String, Value>,
    key: &str,
) -> Result<&'a Map<String, Value>, Error> {
    as_object(value.get(key))
        .ok_or_else(|| invalid(format!("Expected object \"{key}\" in template spec.")))
}

/// The JavaScript `x && typeof x === "object" && !Array.isArray(x)` test.
fn as_object(value: Option<&Value>) -> Option<&Map<String, Value>> {
    value.and_then(Value::as_object)
}

/// `specLoader.ts:371-377`.
fn read_string(value: &Map<String, Value>, key: &str) -> Result<String, Error> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            invalid(format!(
                "Expected non-empty string \"{key}\" in template spec."
            ))
        })
}

/// `specLoader.ts:379-382`. Any string, empty included.
fn read_optional_string(value: &Map<String, Value>, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

/// `specLoader.ts:384-387`.
fn read_optional_number(value: &Map<String, Value>, key: &str) -> Option<f64> {
    value.get(key).and_then(Value::as_f64)
}

fn read_optional_bool(
    value: &Map<String, Value>,
    key: &str,
    at: &str,
) -> Result<Option<bool>, Error> {
    match value.get(key) {
        None => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(invalid(format!("Expected boolean \"{at}.{key}\"."))),
    }
}

/// `specLoader.ts:389-401`.
fn read_optional_string_array(
    value: &Map<String, Value>,
    key: &str,
) -> Result<Option<Vec<String>>, Error> {
    let Some(candidate) = value.get(key) else {
        return Ok(None);
    };
    candidate
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .map(|item| item.as_str().map(str::to_string))
                .collect::<Option<Vec<_>>>()
        })
        .map(Some)
        .ok_or_else(|| invalid(format!("Expected string array \"{key}\" in template spec.")))
}

/// `specLoader.ts:403-420`.
fn read_optional_string_record(
    value: &Map<String, Value>,
    key: &str,
) -> Result<Option<BTreeMap<String, String>>, Error> {
    let Some(candidate) = value.get(key) else {
        return Ok(None);
    };
    let Some(record) = as_object(Some(candidate)) else {
        return Err(invalid(format!(
            "Expected string record \"{key}\" in template spec."
        )));
    };
    record
        .iter()
        .map(|(entry_key, entry_value)| {
            entry_value
                .as_str()
                .map(|s| (entry_key.clone(), s.to_string()))
                .ok_or_else(|| invalid(format!("Expected string values in \"{key}\".")))
        })
        .collect::<Result<BTreeMap<_, _>, Error>>()
        .map(Some)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_missing_recipe_names_the_path_and_hanvil_init() {
        let path = std::env::temp_dir().join("hanvil-no-such-recipe/spec.yaml");
        let error = super::load(&path).expect_err("missing file");
        assert_eq!(
            error.to_string(),
            format!(
                "no recipe at {}. Pass the recipe path, or bootstrap one with `hanvil init`.",
                path.display()
            )
        );
    }

    use super::*;

    const SPEC_PATH: &str = "/work/project/.harness/spec.yaml";

    /// `test/spec-schema.test.mjs:11-15`, the minimal recipe every case starts from.
    const MINIMAL: &str = "schemaVersion: 3\nname: t\nbaseline:\n  commands:\n    - name: install\n      command: \"true\"\n";

    fn load(yaml: &str) -> Result<Loaded, Error> {
        parse(yaml, Path::new(SPEC_PATH))
    }

    fn error_of(yaml: &str) -> String {
        match load(yaml) {
            Err(error) => error.to_string(),
            Ok(loaded) => panic!("expected an error, loaded {:?}", loaded.spec.name),
        }
    }

    fn spec_of(yaml: &str) -> Spec {
        load(yaml).expect("recipe loads").spec
    }

    #[test]
    fn the_minimal_recipe_gets_every_default() {
        let spec = spec_of(MINIMAL);
        assert_eq!(spec.project_root, PathBuf::from("/work/project"));
        assert_eq!(
            spec.prd_paths,
            vec![PathBuf::from("/work/project/.harness/prd.md")]
        );
        assert_eq!(spec.eval_paths, None);
        assert_eq!(spec.agent, AgentPreset::Claude);
        assert_eq!(spec.generator.command, "claude");
        assert_eq!(
            spec.generator
                .args
                .as_deref()
                .and_then(<[String]>::last)
                .map(String::as_str),
            Some("opus")
        );
        assert_eq!(spec.generator.timeout_ms, Some(3_600_000));
        assert_eq!(spec.validator, None);
        assert_eq!(
            spec.validators.static_path,
            PathBuf::from("/work/project/.harness/validators/static.json")
        );
        assert_eq!(
            spec.validators.commands_path,
            PathBuf::from("/work/project/.harness/validators/yarn.json")
        );
        assert_eq!(spec.validators.playwright_path, None);
        assert_eq!(spec.forbidden_files, vec![".env"]);
        assert_eq!(spec.secret_scan.fail_on_files, vec![".env"]);
        assert_eq!(spec.secret_scan.patterns[0].name, "private-key-assignment");
        assert_eq!(
            spec.constraints.forbidden_commands,
            vec![
                "yarn install",
                "yarn run",
                "npm install",
                "npm run",
                "pnpm install",
                "pnpm run"
            ]
        );
        assert_eq!(spec.max_attempts, 3);
        assert_eq!(spec.chain_validation, None);
        assert_eq!(
            spec.jsonl_log_path,
            PathBuf::from("/work/project/.harness/runs/harness.log.jsonl")
        );
        assert!(!spec.validator_enabled());
    }

    #[test]
    fn schema_version_errors_are_upstreams() {
        let cases = [
            (
                "name: t\n",
                format!("{SPEC_PATH} is missing schemaVersion. Set schemaVersion: 3."),
            ),
            (
                "schemaVersion: \"3\"\nname: t\n",
                format!("\"schemaVersion\" must be a positive integer in {SPEC_PATH} (got \"3\")."),
            ),
            (
                "schemaVersion: 0\nname: t\n",
                format!("\"schemaVersion\" must be a positive integer in {SPEC_PATH} (got 0)."),
            ),
            (
                "schemaVersion: 4\nname: t\n",
                format!(
                    "{SPEC_PATH} declares schemaVersion 4, but this harness understands up to 3. \
                     Upgrade the harness: npm install hedera-harness@latest"
                ),
            ),
            (
                "schemaVersion: 2\nname: t\n",
                format!(
                    "{SPEC_PATH} declares schemaVersion 2, which this harness no longer supports. \
                     Set schemaVersion: 3."
                ),
            ),
        ];
        for (yaml, expected) in cases {
            assert_eq!(error_of(yaml), expected, "for {yaml:?}");
        }
    }

    #[test]
    fn removed_keys_fail_with_their_hints() {
        let yaml = format!("{MINIMAL}contract: x\nlogging: y\n");
        assert_eq!(
            error_of(&yaml),
            format!(
                "{SPEC_PATH} uses removed key(s): contract, logging. use eval: not contract: \
                 remove logging: — harness logs always live under .harness/runs/"
            )
        );
    }

    #[test]
    fn unknown_top_level_keys_warn_and_nested_ones_do_not() {
        let loaded = load(&format!(
            "{MINIMAL}mystery: 1\ngenerator:\n  provider: command\n  command: bash\n"
        ))
        .expect("loads");
        assert_eq!(
            loaded.warnings,
            vec![
                "ignoring unknown key(s): mystery. If these come from a newer recipe, upgrade the harness."
            ]
        );
        assert_eq!(loaded.spec.generator.command, "bash");
        assert_eq!(loaded.spec.generator.args, None);
    }

    #[test]
    fn baseline_must_name_an_install_command() {
        let cases = [
            (
                "schemaVersion: 3\nname: t\n",
                "run requires baseline.commands including a command literally named \"install\" \
                 (used for host-health checks and install fingerprinting).",
            ),
            (
                "schemaVersion: 3\nname: t\nbaseline: {}\n",
                "run requires baseline.commands including a command literally named \"install\" \
                 (used for host-health checks and install fingerprinting).",
            ),
            (
                "schemaVersion: 3\nname: t\nbaseline:\n  commands:\n    - yarn install\n",
                "baseline.commands must include a command literally named \"install\" \
                 (used for host-health checks and install fingerprinting).",
            ),
            (
                "schemaVersion: 3\nname: t\nbaseline: 4\n",
                "Expected object \"baseline\" in template spec.",
            ),
            (
                "schemaVersion: 3\nname: t\nbaseline:\n  commands: x\n",
                "Expected array \"baseline.commands\" in template spec.",
            ),
            (
                "schemaVersion: 3\nname: t\nbaseline:\n  commands:\n    - 3\n",
                "Expected string or object at baseline.commands[0].",
            ),
        ];
        for (yaml, expected) in cases {
            assert_eq!(error_of(yaml), expected, "for {yaml:?}");
        }
    }

    #[test]
    fn prd_and_eval_lists_follow_the_one_to_one_rule() {
        let cases = [
            (
                "prd: \"  \"\n",
                "Expected non-empty string \"prd\" in template spec.",
            ),
            (
                "prd: [a, 2]\n",
                "Expected \"prd\" to be a path or a non-empty list of paths.",
            ),
            (
                "prd: []\n",
                "Expected \"prd\" to list at least one PRD path.",
            ),
            (
                "eval: \"\"\n",
                "Expected non-empty string \"eval\" in template spec.",
            ),
            (
                "eval: 7\n",
                "Expected \"eval\" to be a path or a non-empty list of paths.",
            ),
            ("eval: []\n", "Expected \"eval\" to list at least one path."),
            (
                "prd: [a.md, b.md]\neval: [e.json]\n",
                "\"eval\" list has 1 path(s) but \"prd\" has 2; list form must be 1:1.",
            ),
        ];
        for (extra, expected) in cases {
            assert_eq!(
                error_of(&format!("{MINIMAL}{extra}")),
                expected,
                "for {extra:?}"
            );
        }
        let spec = spec_of(&format!("{MINIMAL}prd: [a.md, /abs/b.md]\neval: e.json\n"));
        assert_eq!(
            spec.prd_paths,
            vec![
                PathBuf::from("/work/project/a.md"),
                PathBuf::from("/abs/b.md")
            ]
        );
        assert_eq!(
            spec.eval_paths,
            Some(vec![PathBuf::from("/work/project/e.json")])
        );
    }

    #[test]
    fn agent_presets_carry_the_upstream_argv() {
        let spec = spec_of(&format!(
            "{MINIMAL}agent: cursor\nvalidator:\n  enabled: true\n"
        ));
        assert_eq!(spec.agent, AgentPreset::Cursor);
        assert_eq!(spec.generator.command, "agent");
        let args = spec.generator.args.clone().unwrap_or_default();
        assert_eq!(&args[..2], ["-p", "--trust"]);
        assert_eq!(&args[args.len() - 2..], ["--model", "composer-2.5"]);
        assert!(!args.iter().any(|a| a == "{prompt}"));
        let validator = spec.validator.clone().expect("validator");
        assert!(validator.enabled);
        assert_eq!(validator.command.args, spec.generator.args);

        let spec = spec_of(&format!("{MINIMAL}validator:\n  enabled: true\n"));
        let validator = spec.validator.clone().expect("validator");
        let args = validator.command.args.unwrap_or_default();
        assert_eq!(
            &args[2..4],
            ["--allowedTools", "mcp__playwright,Read,Grep,Glob"]
        );
        assert_eq!(&args[args.len() - 2..], ["--model", "opus"]);
        assert!(spec.validator_enabled());

        assert_eq!(
            error_of(&format!("{MINIMAL}agent: codex\n")),
            "Unknown agent preset \"codex\". Available: cursor, claude."
        );
    }

    #[test]
    fn a_disabled_validator_keeps_a_stub() {
        let spec = spec_of(&format!("{MINIMAL}validator:\n  enabled: false\n"));
        let validator = spec.validator.clone().expect("stub");
        assert!(!validator.enabled);
        assert_eq!(validator.command.command, "claude");
        assert_eq!(validator.command.args, None);
        assert!(!spec.validator_enabled());
        assert_eq!(
            error_of(&format!("{MINIMAL}validator: yes\n")),
            "Expected object \"validator\" in template spec."
        );
    }

    #[test]
    fn constraints_derive_forbidden_commands_from_the_package_manager() {
        let spec = spec_of(&format!(
            "{MINIMAL}constraints:\n  packageManager: Yarn\n  workspaces: [packages/app]\n"
        ));
        assert_eq!(
            spec.constraints.forbidden_commands,
            vec!["npm install", "npm run", "pnpm install", "pnpm run"]
        );
        assert_eq!(spec.forbidden_files, vec![".env", "packages/app/.env"]);
        assert_eq!(
            spec.secret_scan.fail_on_files,
            vec![".env", "packages/app/.env"]
        );
        assert_eq!(
            error_of(&format!("{MINIMAL}constraints:\n  workspaces: [1]\n")),
            "Expected string array \"workspaces\" in template spec."
        );
    }

    #[test]
    fn chain_validation_local_needs_no_operator_and_defaults_its_urls() {
        let spec = spec_of(&format!(
            "{MINIMAL}chainValidation:\n  enabled: true\n  network: local\n"
        ));
        let chain = spec.chain_validation.expect("chain");
        assert_eq!(chain.network, ChainNetwork::Local);
        assert_eq!(chain.operator_account_id_env, "HEDERA_OPERATOR_ID");
        assert_eq!(chain.operator_private_key_env, "HEDERA_OPERATOR_KEY");
        assert_eq!(
            chain.local,
            Some(ChainLocal {
                rpc_url: "http://localhost:7546".into(),
                grpc_url: "localhost:50211".into(),
                mirror_url: "http://localhost:5551".into(),
            })
        );
        assert_eq!(chain.funding_hbar, 10.0);
        assert!(chain.sweep_back);
        assert_eq!(chain.browser_local_storage_key, "burnerWallet.pk");
        assert!(chain.snapshot_per_attempt);
        assert_eq!(chain.advance_time_seconds, 0);
        assert!(chain.assertions.is_empty());

        let spec = spec_of(&format!(
            "{MINIMAL}chainValidation:\n  network: local\n  local:\n    rpcUrl: http://127.0.0.1:9\n  snapshotPerAttempt: false\n  advanceTimeSeconds: 86400\n"
        ));
        let chain = spec.chain_validation.expect("chain");
        assert_eq!(
            chain.local.as_ref().map(|l| l.rpc_url.as_str()),
            Some("http://127.0.0.1:9")
        );
        assert_eq!(
            chain.local.as_ref().map(|l| l.grpc_url.as_str()),
            Some("localhost:50211")
        );
        assert!(!chain.snapshot_per_attempt);
        assert_eq!(chain.advance_time_seconds, 86_400);
    }

    #[test]
    fn chain_validation_errors_are_upstreams() {
        let cases = [
            (
                "network: mainnet\n",
                "chainValidation.network must be \"testnet\" or \"local\" (got \"mainnet\"). Mainnet is not allowed.",
            ),
            (
                "network: testnet\n",
                "Expected object \"operator\" in template spec.",
            ),
            (
                "network: testnet\n  operator: {accountIdEnv: A}\n",
                "Expected non-empty string \"privateKeyEnv\" in template spec.",
            ),
            (
                "network: testnet\n  operator: {accountIdEnv: A, privateKeyEnv: B}\n  local: {}\n",
                "chainValidation.local is only valid with network: \"local\".",
            ),
            (
                "network: local\n  fundingHbar: 0\n",
                "Expected positive number \"chainValidation.fundingHbar\".",
            ),
            (
                "network: local\n  deploy: {commands: x}\n",
                "Expected array \"chainValidation.deploy.commands\" in template spec.",
            ),
            (
                "network: local\n  deploy: {commands: [x]}\n",
                "Expected object at chainValidation.deploy.commands[0].",
            ),
            (
                "network: local\n  advanceTimeSeconds: -1\n",
                "Expected non-negative integer \"chainValidation.advanceTimeSeconds\".",
            ),
        ];
        for (body, expected) in cases {
            let yaml = format!("{MINIMAL}chainValidation:\n  {body}");
            assert_eq!(error_of(&yaml), expected, "for {body:?}");
        }
        assert_eq!(
            spec_of(&format!(
                "{MINIMAL}chainValidation:\n  enabled: false\n  network: mainnet\n"
            ))
            .chain_validation,
            None
        );
    }

    #[test]
    fn chain_assertions_parse_and_refuse_the_unverifiable() {
        let spec = spec_of(&format!(
            "{MINIMAL}chainValidation:\n  network: local\n  assert:\n    - account: signer\n      minBalanceHbar: 9\n    - account: 0.0.1002\n      exists: true\n    - contract: 0x{}\n    - topic: created\n      messagesAtLeast: 3\n    - transactions:\n        type: CONSENSUSSUBMITMESSAGE\n        payer: signer\n        atLeast: 2\n",
            "ab".repeat(20)
        ));
        let assertions = spec.chain_validation.expect("chain").assertions;
        assert_eq!(assertions.len(), 5);
        assert_eq!(
            assertions[0],
            ChainAssertion::Account {
                account: AccountRef::Signer,
                min_balance_hbar: Some(9.0),
                exists: None,
                deleted: None
            }
        );
        assert_eq!(
            assertions[1],
            ChainAssertion::Account {
                account: AccountRef::Id("0.0.1002".into()),
                min_balance_hbar: None,
                exists: Some(true),
                deleted: None
            }
        );
        assert!(matches!(
            &assertions[2],
            ChainAssertion::Contract { deployed: true, .. }
        ));
        assert_eq!(
            assertions[3],
            ChainAssertion::Topic {
                topic: TopicRef::Created,
                messages_at_least: 3
            }
        );
        assert_eq!(
            assertions[4],
            ChainAssertion::Transactions {
                kind: "CONSENSUSSUBMITMESSAGE".into(),
                payer: Some(AccountRef::Signer),
                at_least: 2
            }
        );

        // Rejections: an empty object means none are tolerated, and the filters are optional.
        let rejections = spec_of(&format!(
            "{MINIMAL}chainValidation:\n  network: local\n  assert:\n    - rejections: {{}}\n    - rejections:\n        atMost: 2\n        type: CONSENSUSSUBMITMESSAGE\n        payer: signer\n"
        ))
        .chain_validation
        .expect("chain")
        .assertions;
        assert_eq!(
            rejections[0],
            ChainAssertion::Rejections {
                kind: None,
                payer: None,
                at_most: 0
            }
        );
        assert_eq!(
            rejections[1],
            ChainAssertion::Rejections {
                kind: Some("CONSENSUSSUBMITMESSAGE".into()),
                payer: Some(AccountRef::Signer),
                at_most: 2
            }
        );

        let cases = [
            ("- 4\n", "Expected object at chainValidation.assert[0]."),
            (
                "- {account: signer, topic: created}\n",
                "chainValidation.assert[0] must have exactly one of: account, contract, topic, transactions, rejections.",
            ),
            (
                "- {account: signer}\n",
                "chainValidation.assert[0].account needs minBalanceHbar, exists or deleted.",
            ),
            (
                "- {account: bob, exists: true}\n",
                "chainValidation.assert[0].account must be \"signer\", an id like 0.0.N, or a 0x address.",
            ),
            (
                // Quoted: YAML 1.1 reads a short unquoted `0x12` as the integer 18.
                "- {contract: \"0x12\"}\n",
                "chainValidation.assert[0].contract must be a 0x-prefixed 20-byte address.",
            ),
            (
                "- {topic: created}\n",
                "Expected non-negative integer \"chainValidation.assert[0].messagesAtLeast\".",
            ),
            (
                "- {transactions: {type: CONTRACTCALL}}\n",
                "chainValidation.assert[0].transactions.type must be one of: CRYPTOCREATEACCOUNT, CRYPTOTRANSFER, CRYPTODELETE, CONSENSUSCREATETOPIC, CONSENSUSSUBMITMESSAGE, ETHEREUMTRANSACTION.",
            ),
            (
                "- {transactions: {type: CRYPTOTRANSFER, atLeast: 0}}\n",
                "Expected positive integer \"chainValidation.assert[0].transactions.atLeast\".",
            ),
            (
                "- {rejections: 0}\n",
                "Expected object \"chainValidation.assert[0].rejections\".",
            ),
            (
                "- {rejections: {type: CONTRACTCALL}}\n",
                "chainValidation.assert[0].rejections.type must be one of: CRYPTOCREATEACCOUNT, CRYPTOTRANSFER, CRYPTODELETE, CONSENSUSCREATETOPIC, CONSENSUSSUBMITMESSAGE, ETHEREUMTRANSACTION.",
            ),
            (
                "- {rejections: {atMost: -1}}\n",
                "Expected non-negative integer \"chainValidation.assert[0].rejections.atMost\".",
            ),
            (
                "- {rejections: {payer: bob}}\n",
                "chainValidation.assert[0].payer must be \"signer\", an id like 0.0.N, or a 0x address.",
            ),
        ];
        for (body, expected) in cases {
            let yaml =
                format!("{MINIMAL}chainValidation:\n  network: local\n  assert:\n    {body}");
            assert_eq!(error_of(&yaml), expected, "for {body:?}");
        }
        assert_eq!(
            error_of(&format!(
                "{MINIMAL}chainValidation:\n  network: local\n  assert: no\n"
            )),
            "Expected array \"chainValidation.assert\" in template spec."
        );
    }

    #[test]
    fn secret_scan_falls_back_silently_but_refuses_a_shapeless_pattern() {
        let spec = spec_of(&format!(
            "{MINIMAL}secretScan:\n  failOnFiles: nope\n  patterns:\n    - name: k\n      pattern: KEY=.*\n      allowIn: [README.md]\n"
        ));
        assert_eq!(spec.secret_scan.fail_on_files, vec![".env"]);
        assert_eq!(spec.secret_scan.patterns.len(), 1);
        assert_eq!(spec.secret_scan.patterns[0].allow_in, vec!["README.md"]);
        assert_eq!(
            error_of(&format!(
                "{MINIMAL}secretScan:\n  patterns:\n    - just-a-string\n"
            )),
            "Expected {name, pattern} objects in \"secretScan.patterns\"."
        );
    }

    #[test]
    fn generator_and_validator_blocks_read_env_and_timeouts() {
        let spec = spec_of(&format!(
            "{MINIMAL}generator:\n  command: bash\n  args: [-c, \"echo\"]\n  env: {{A: b}}\n  timeoutMs: 5000\nvalidator:\n  command: bash\n  args: [-c, \"echo\"]\n"
        ));
        assert_eq!(spec.generator.timeout_ms, Some(5000));
        assert_eq!(
            spec.generator
                .env
                .as_ref()
                .and_then(|e| e.get("A"))
                .map(String::as_str),
            Some("b")
        );
        assert!(spec.validator_enabled());
        assert_eq!(
            error_of(&format!(
                "{MINIMAL}generator:\n  command: bash\n  env: {{A: 1}}\n"
            )),
            "Expected string values in \"env\"."
        );
        assert_eq!(
            error_of(&format!("{MINIMAL}generator: {{}}\n")),
            "Expected non-empty string \"command\" in template spec."
        );
    }

    #[test]
    fn an_empty_file_is_a_recipe_missing_its_schema_version() {
        assert_eq!(
            error_of(""),
            format!("{SPEC_PATH} is missing schemaVersion. Set schemaVersion: 3.")
        );
        assert!(error_of("- a list\n").contains("is not a mapping"));
    }
}
