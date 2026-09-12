//! Boot the three listeners on one chain. `main` uses it for the bare node; `harness::run` uses
//! it to put a network under the agent in the same process.

use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::RwLock;

use crate::cli::NodeArgs;
use crate::serve::{Bound, Shared};
use crate::state::{Chain, Clock};
use crate::{hapi, mirror, rpc};

/// A running node: the chain and its three listeners.
pub(crate) struct Node {
    /// The one chain every listener reads and writes.
    pub(crate) shared: Shared,
    /// The clock the listeners stamp transactions with.
    pub(crate) clock: Arc<dyn Clock>,
    /// JSON-RPC listener.
    pub(crate) rpc: Bound,
    /// Mirror REST listener.
    pub(crate) mirror: Bound,
    /// HAPI gRPC listener.
    pub(crate) grpc: Bound,
}

/// What can stop a node from coming up.
#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    /// A listener could not bind. The address and the flag are in the message because a port
    /// already taken by another hanvil is the usual cause. `hanvil validate` boots a node for any
    /// recipe with `chainValidation`, so this is reachable without ever running `hanvil` itself.
    #[error(
        "binding {listener} on {host}:{port}: {source}; another hanvil may hold this port — pass \
         --port 0 --mirror-port 0 --grpc-port 0 for ephemeral ones"
    )]
    Bind {
        /// Which listener.
        listener: &'static str,
        /// Requested interface.
        host: String,
        /// Requested port; 0 means any.
        port: u16,
        /// The OS error.
        #[source]
        source: std::io::Error,
    },
    /// `--state` named a file that could not be read.
    #[error("reading chain state from {path}: {source}")]
    ReadState {
        /// The file.
        path: PathBuf,
        /// The OS error.
        #[source]
        source: std::io::Error,
    },
    /// `--state` named a file that is not a chain dump.
    #[error("parsing chain state from {path}: {source}")]
    ParseState {
        /// The file.
        path: PathBuf,
        /// The decode error.
        #[source]
        source: crate::state::Error,
    },
    /// Genesis could not be built from the flags.
    #[error("building genesis state: {0}")]
    Genesis(#[source] crate::state::Error),
}

/// The chain the flags ask for: the `--state` file when it exists, genesis otherwise. Genesis
/// flags are ignored when the file exists — the state in it decides the chain id and the
/// accounts.
pub(crate) fn load_or_genesis(args: &NodeArgs, clock: &dyn Clock) -> Result<Chain, Error> {
    // The flag is applied after the load so it wins over whatever the file was written with.
    let mut chain = read_or_genesis(args, clock)?;
    chain.set_max_rejections(args.max_rejections);
    Ok(chain)
}

fn read_or_genesis(args: &NodeArgs, clock: &dyn Clock) -> Result<Chain, Error> {
    match args.state.as_ref().filter(|path| path.exists()) {
        Some(path) => {
            let json = std::fs::read_to_string(path).map_err(|source| Error::ReadState {
                path: path.clone(),
                source,
            })?;
            Chain::from_json(&json).map_err(|source| Error::ParseState {
                path: path.clone(),
                source,
            })
        }
        None => Chain::genesis(&args.genesis(clock.now())).map_err(Error::Genesis),
    }
}

impl Node {
    /// Bind the three listeners in the order the banner prints them. Each is accepting
    /// connections before this returns.
    pub(crate) async fn boot(
        args: &NodeArgs,
        chain: Chain,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, Error> {
        let shared: Shared = Arc::new(RwLock::new(chain));
        let bind_error = |listener: &'static str, port: u16| {
            move |source| Error::Bind {
                listener,
                host: args.host.clone(),
                port,
                source,
            }
        };

        let rpc = rpc::serve(
            rpc::App {
                chain: Arc::clone(&shared),
                clock: Arc::clone(&clock),
            },
            &args.host,
            args.port,
        )
        .await
        .map_err(bind_error("JSON-RPC", args.port))?;
        let mirror = mirror::serve(Arc::clone(&shared), &args.host, args.mirror_port)
            .await
            .map_err(bind_error("mirror REST", args.mirror_port))?;
        let grpc = hapi::serve(
            hapi::Node {
                chain: Arc::clone(&shared),
                clock: Arc::clone(&clock),
                verify_signatures: !args.no_sig_verify,
            },
            &args.host,
            args.grpc_port,
        )
        .await
        .map_err(bind_error("HAPI gRPC", args.grpc_port))?;

        Ok(Self {
            shared,
            clock,
            rpc,
            mirror,
            grpc,
        })
    }

    /// Stop accepting connections. In-flight requests are dropped with their tasks.
    pub(crate) fn shutdown(self) {
        self.rpc.task.abort();
        self.mirror.task.abort();
        self.grpc.task.abort();
    }
}
