#![forbid(unsafe_code)]
//! hanvil — a local Hedera network in one binary.
//!
//! One in-memory chain, three listeners: JSON-RPC (relay shape), mirror REST, HAPI gRPC.
//! See docs/code-plan.md for the architecture and .claude/CLAUDE.md for the rules.

mod cli;
mod evm;
mod hapi;
mod harness;
mod keys;
mod mirror;
mod node;
mod rpc;
mod serve;
mod state;
mod toll;

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Context as _;
use clap::Parser;

#[tokio::main]
async fn main() -> anyhow::Result<ExitCode> {
    let started = Instant::now();
    let args = cli::Args::parse();
    // A harness run prints its own stage lines; the node's per-transaction log would drown
    // them. `RUST_LOG` still turns it on.
    init_tracing(args.node.silent || args.command.is_some());
    match args.command {
        None => serve_node(&args.node, started)
            .await
            .map(|()| ExitCode::SUCCESS),
        Some(command) => Ok(harness::dispatch(command, args.node).await),
    }
}

/// The bare node: boot, print the banner, run until ctrl-c, dump state if asked.
async fn serve_node(args: &cli::NodeArgs, started: Instant) -> anyhow::Result<()> {
    let clock: Arc<dyn state::Clock> = Arc::new(state::time::SystemClock);
    let chain = node::load_or_genesis(args, clock.as_ref())?;
    let node = node::Node::boot(args, chain, clock).await?;

    if !args.silent {
        cli::banner(
            args,
            &node.shared.read(),
            node.rpc.local_addr,
            node.mirror.local_addr,
            node.grpc.local_addr,
            started.elapsed(),
        );
    }

    let mining = args.block_time.map(|seconds| {
        mine_on_interval(Arc::clone(&node.shared), Arc::clone(&node.clock), seconds)
    });

    tokio::signal::ctrl_c()
        .await
        .context("waiting for ctrl-c")?;
    tracing::info!("shutting down");
    if let Some(task) = mining {
        task.abort();
    }
    let shared = Arc::clone(&node.shared);
    node.shutdown();

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
