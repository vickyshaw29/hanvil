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
    let chain = match args.state.as_ref().filter(|path| path.exists()) {
        Some(path) => {
            let json = std::fs::read_to_string(path)
                .with_context(|| format!("reading chain state from {}", path.display()))?;
            state::Chain::from_json(&json)
                .with_context(|| format!("parsing chain state from {}", path.display()))?
        }
        None => {
            state::Chain::genesis(&args.genesis(clock.now())).context("building genesis state")?
        }
    };
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

    let mining = args
        .block_time
        .map(|seconds| mine_on_interval(Arc::clone(&shared), Arc::clone(&grpc_clock), seconds));

    tokio::signal::ctrl_c()
        .await
        .context("waiting for ctrl-c")?;
    tracing::info!("shutting down");
    if let Some(task) = mining {
        task.abort();
    }
    rpc.task.abort();
    mirror.task.abort();
    grpc.task.abort();

    if let Some(path) = args.dump_path() {
        let json = shared.read().to_json().context("serialising chain state")?;
        std::fs::write(path, json)
            .with_context(|| format!("writing chain state to {}", path.display()))?;
        if !args.silent {
            println!("Wrote chain state to {}", path.display());
        }
    }
    Ok(())
}

/// Mine an empty block every `seconds`. The lock is taken and released inside the loop, never
/// held across the sleep.
fn mine_on_interval(
    chain: serve::Shared,
    clock: Arc<dyn state::Clock>,
    seconds: u64,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let period = std::time::Duration::from_secs(seconds.max(1));
        let mut ticker = tokio::time::interval(period);
        ticker.tick().await; // the first tick is immediate
        loop {
            ticker.tick().await;
            let number = {
                let mut chain = chain.write();
                chain.mine_empty(clock.now()).number
            };
            tracing::info!(block = number, "mined an empty block on the interval");
        }
    })
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
