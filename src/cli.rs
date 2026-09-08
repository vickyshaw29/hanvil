//! Command line surface. Reads like Anvil's on purpose.

use std::net::SocketAddr;
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
    /// Interface to bind. 0.0.0.0 exposes the chain beyond this machine.
    #[arg(long, default_value = "127.0.0.1", env = "HANVIL_HOST")]
    pub host: String,

    /// JSON-RPC port, in the relay's shape. 0 picks a free port.
    #[arg(long, short = 'p', default_value_t = 7546, env = "HANVIL_PORT")]
    pub port: u16,

    /// Mirror node REST port. 0 picks a free port.
    #[arg(long, default_value_t = 5551, env = "HANVIL_MIRROR_PORT")]
    pub mirror_port: u16,

    /// HAPI gRPC port. 0 picks a free port.
    #[arg(long, default_value_t = 50211, env = "HANVIL_GRPC_PORT")]
    pub grpc_port: u16,

    /// EVM chain id. 298 is Hedera local, 296 testnet, 31337 mimics hardhat.
    #[arg(long, default_value_t = 298, env = "HANVIL_CHAIN_ID")]
    pub chain_id: u64,

    /// Predefined accounts per key type (ECDSA, ECDSA-alias, ED25519). Max 10.
    #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u8).range(1..=10))]
    pub accounts: u8,

    /// Starting balance of each predefined account, in HBAR.
    #[arg(long, default_value_t = 10_000)]
    pub balance: u64,

    /// Gas price in tinybar per gas. Offering less is refused, as on the relay; fees go to
    /// 0.0.98.
    #[arg(long, default_value_t = 71, env = "HANVIL_GAS_PRICE")]
    pub gas_price: u64,

    /// Accept HAPI transactions without checking their signatures. Useful when replaying a body
    /// signed for another network; every other check still runs.
    #[arg(long)]
    pub no_sig_verify: bool,

    /// Print nothing.
    #[arg(long)]
    pub silent: bool,
}

impl Args {
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
    args: &Args,
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
