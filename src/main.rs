#![forbid(unsafe_code)]
//! hanvil — a local Hedera network in one binary.
//!
//! One in-memory chain, three listeners: JSON-RPC (relay shape), mirror REST, HAPI gRPC.
//! See docs/code-plan.md for the architecture and .claude/CLAUDE.md for the rules.

mod cli;
mod evm;
mod hapi;
mod keys;
mod mirror;
mod rpc;
mod serve;
mod state;

use std::sync::Arc;
use std::time::Instant;

use anyhow::Context as _;
use clap::Parser;
use parking_lot::RwLock;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let started = Instant::now();
    let args = cli::Args::parse();
    init_tracing(args.silent);

    let clock: Arc<dyn state::Clock> = Arc::new(state::time::SystemClock);
    let chain =
        state::Chain::genesis(&args.genesis(clock.now())).context("building genesis state")?;
    let shared: serve::Shared = Arc::new(RwLock::new(chain));

    let grpc_clock = Arc::clone(&clock);
    let rpc = rpc::serve(
        rpc::App {
            chain: Arc::clone(&shared),
            clock,
        },
        &args.host,
        args.port,
    )
    .await
    .context("starting JSON-RPC listener")?;
    let mirror = mirror::serve(Arc::clone(&shared), &args.host, args.mirror_port)
        .await
        .context("starting mirror REST listener")?;
    let grpc = hapi::serve(
        hapi::Node {
            chain: Arc::clone(&shared),
            clock: Arc::clone(&grpc_clock),
            verify_signatures: !args.no_sig_verify,
        },
        &args.host,
        args.grpc_port,
    )
    .await
    .context("starting HAPI gRPC listener")?;

    if !args.silent {
        cli::banner(
            &args,
            &shared.read(),
            rpc.local_addr,
            mirror.local_addr,
            grpc.local_addr,
            started.elapsed(),
        );
    }

    tokio::signal::ctrl_c()
        .await
        .context("waiting for ctrl-c")?;
    tracing::info!("shutting down");
    rpc.task.abort();
    mirror.task.abort();
    grpc.task.abort();
    Ok(())
}

fn init_tracing(silent: bool) {
    use tracing_subscriber::EnvFilter;
    let default = if silent { "off" } else { "hanvil=info" };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .without_time()
        .init();
}
