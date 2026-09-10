//! Command line surface. Reads like Anvil's on purpose.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;

use crate::evm::units::Tinybar;
use crate::state::{Chain, Genesis, Timestamp};

/// Local Hedera network: JSON-RPC, mirror REST and HAPI gRPC from one in-memory chain.
#[derive(Parser, Debug, Clone)]
#[command(
    name = "hanvil",
    version,
    about,
    long_about = "One in-memory Hedera chain behind three listeners: the JSON-RPC relay on 7546, \
                  the mirror node REST API on 5551, and HAPI gRPC on 50211 — the ports \
                  hiero-local-node uses, with the same thirty predefined accounts. State is not \
                  persisted; `evm_snapshot` and `evm_revert` put it back."
)]
pub struct Args {
    /// Flags of the node itself. They are global so a subcommand that boots the node in-process
    /// accepts them after its own name.
    #[command(flatten)]
    pub node: NodeArgs,

    /// The harness. Bare `hanvil` is the node.
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Harness subcommands, named as in `hedera-harness`.
#[derive(clap::Subcommand, Debug, Clone)]
pub enum Command {
    /// Drive a coding agent against a recipe on the in-process chain, attempt by attempt.
    Run(RunArgs),
    /// Check the recipe and the host before a long run. Reports everything at once.
    Doctor(DoctorArgs),
    /// ASSERT, then the thin SMOKE gate when ASSERT is clean. No agent, no chain.
    Validate(ValidateArgs),
}

/// `hanvil validate [SPEC] [--workspace DIR]`.
#[derive(clap::Args, Debug, Clone)]
pub struct ValidateArgs {
    /// Recipe to validate against. Defaults to .harness/spec.yaml.
    #[arg(value_name = "SPEC")]
    pub spec: Option<PathBuf>,

    /// Project directory. Defaults to the current directory.
    #[arg(long, value_name = "DIR")]
    pub workspace: Option<PathBuf>,
}

/// `hanvil run [SPEC] [--max-attempts N] [--new | --continue BRANCH] [--workspace DIR]`.
///
/// The node boots on the recipe's `chainValidation.local` ports, or 7546/5551/50211. A node
/// flag given explicitly wins; one left at its default defers to the recipe.
#[derive(clap::Args, Debug, Clone)]
pub struct RunArgs {
    /// Recipe to run. Defaults to .harness/spec.yaml.
    #[arg(value_name = "SPEC")]
    pub spec: Option<PathBuf>,

    /// Attempt budget per increment. Overrides HARNESS_MAX_ATTEMPTS and the recipe.
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u64).range(1..))]
    pub max_attempts: Option<u64>,

    /// Start a fresh harness/run-* branch even when the current one matches the recipe.
    #[arg(long, conflicts_with = "continue_branch")]
    pub new: bool,

    /// Check out this harness/run-* branch and resume its session.
    #[arg(long = "continue", value_name = "BRANCH")]
    pub continue_branch: Option<String>,

    /// Project directory the agent edits in place. Defaults to the current directory.
    #[arg(long, value_name = "DIR")]
    pub workspace: Option<PathBuf>,

    /// Do not vendor product skills from hedera-skills.
    #[arg(long)]
    pub no_skills: bool,
}

/// `hanvil doctor [SPEC] [--recipe-only] [--workspace DIR]`.
#[derive(clap::Args, Debug, Clone)]
pub struct DoctorArgs {
    /// Recipe to check. Defaults to .harness/spec.yaml.
    #[arg(value_name = "SPEC")]
    pub spec: Option<PathBuf>,

    /// Check the recipe alone; skip the host and project checks.
    #[arg(long)]
    pub recipe_only: bool,

    /// Project directory for the git and tool checks. Defaults to the current directory.
    #[arg(long, value_name = "DIR")]
    pub workspace: Option<PathBuf>,
}

/// Everything that shapes the chain and its listeners.
#[derive(clap::Args, Debug, Clone)]
pub struct NodeArgs {
    /// Interface to bind. 0.0.0.0 exposes the chain beyond this machine.
    #[arg(long, global = true, default_value = "127.0.0.1", env = "HANVIL_HOST")]
    pub host: String,

    /// JSON-RPC port, in the relay's shape. 0 picks a free port.
    #[arg(
        long,
        short = 'p',
        global = true,
        default_value_t = 7546,
        env = "HANVIL_PORT"
    )]
    pub port: u16,

    /// Mirror node REST port. 0 picks a free port.
    #[arg(
        long,
        global = true,
        default_value_t = 5551,
        env = "HANVIL_MIRROR_PORT"
    )]
    pub mirror_port: u16,

    /// HAPI gRPC port. 0 picks a free port.
    #[arg(long, global = true, default_value_t = 50211, env = "HANVIL_GRPC_PORT")]
    pub grpc_port: u16,

    /// EVM chain id. 298 is Hedera local, 296 testnet, 31337 mimics hardhat.
    #[arg(long, global = true, default_value_t = 298, env = "HANVIL_CHAIN_ID")]
    pub chain_id: u64,

    /// Predefined accounts per key type (ECDSA, ECDSA-alias, ED25519). Max 10.
    #[arg(long, global = true, default_value_t = 10, value_parser = clap::value_parser!(u8).range(1..=10))]
    pub accounts: u8,

    /// Starting balance of each predefined account, in HBAR.
    #[arg(long, global = true, default_value_t = 10_000)]
    pub balance: u64,

    /// Gas price in tinybar per gas. Offering less is refused, as on the relay; fees go to
    /// 0.0.98.
    #[arg(long, global = true, default_value_t = 71, env = "HANVIL_GAS_PRICE")]
    pub gas_price: u64,

    /// Accept HAPI transactions without checking their signatures. Useful when replaying a body
    /// signed for another network; every other check still runs.
    #[arg(long, global = true)]
    pub no_sig_verify: bool,

    /// Mine an empty block every N seconds. Transactions still mine their own block the moment
    /// they arrive — this adds empty blocks so time advances on its own, it does not batch.
    #[arg(long, global = true, value_name = "SECONDS")]
    pub block_time: Option<u64>,

    /// Load the chain from this file at boot, and write it back on exit. Genesis flags are
    /// ignored when the file exists: the state in it decides the chain id and the accounts.
    #[arg(long, global = true, value_name = "FILE")]
    pub state: Option<PathBuf>,

    /// Write the chain to this file on exit. Takes precedence over `--state` for the write.
    #[arg(long, global = true, value_name = "FILE")]
    pub dump_state: Option<PathBuf>,

    /// Print nothing.
    #[arg(long, global = true)]
    pub silent: bool,
}

impl NodeArgs {
    /// Where the chain is written on exit, if anywhere.
    pub fn dump_path(&self) -> Option<&PathBuf> {
        self.dump_state.as_ref().or(self.state.as_ref())
    }

    /// Genesis parameters derived from the flags.
    pub fn genesis(&self, now: Timestamp) -> Genesis {
        Genesis {
            chain_id: self.chain_id,
            accounts_per_type: self.accounts,
            balance: Tinybar::from_hbar(self.balance),
            gas_price: Tinybar(self.gas_price),
            now,
        }
    }
}

/// Boot banner: endpoints, accounts, and how long boot took.
pub fn banner(
    args: &NodeArgs,
    chain: &Chain,
    rpc: SocketAddr,
    mirror: SocketAddr,
    grpc: SocketAddr,
    elapsed: Duration,
) {
    println!(
        "hanvil {} — local Hedera network",
        env!("CARGO_PKG_VERSION")
    );
    println!("JSON-RPC   http://{rpc}   chain id {}", chain.chain_id());
    println!("Mirror     http://{mirror}/api/v1");
    println!("HAPI gRPC  {grpc}          node 0.0.3");
    if args.no_sig_verify {
        println!("           signature verification off (--no-sig-verify)");
    }
    if let Some(seconds) = args.block_time {
        println!("           empty block every {seconds}s (--block-time)");
    }
    if let Some(path) = args.dump_path() {
        println!("           state written to {} on exit", path.display());
    }
    println!();

    for (title, group) in chain.accounts_by_group() {
        println!("{title}");
        for account in group {
            match account.alias {
                Some(alias) => println!(
                    "{}  {}  {}",
                    account.id,
                    alias,
                    account.private_key_hex.as_deref().unwrap_or("-")
                ),
                // Long-zero addresses are printed lowercase: EIP-55 casing on a number reads as noise.
                None => println!(
                    "{}  {:#x}  {}",
                    account.id,
                    account.evm_address(),
                    account.private_key_hex.as_deref().unwrap_or("-")
                ),
            }
        }
        println!();
    }
    println!("Started in {} ms", elapsed.as_millis());
}
