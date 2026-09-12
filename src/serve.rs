//! What the listeners share: the handle on the one chain, and a socket that is already accepting.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use parking_lot::RwLock;
use tokio::task::JoinHandle;

use crate::state::Chain;

/// The chain every listener reads and writes. One lock, taken per request, never held across an
/// `.await`.
pub type Shared = Arc<RwLock<Chain>>;

/// A bound listener.
pub struct Bound {
    /// The address actually bound; the port matters when the caller asked for 0.
    pub local_addr: SocketAddr,
    /// The serving task.
    pub task: JoinHandle<()>,
}

/// Bind `host:port` and serve `router`. Returns once the socket is listening, so the banner never
/// prints a port a client cannot connect to yet.
pub async fn bind(
    host: &str,
    port: u16,
    router: Router,
    name: &'static str,
) -> std::io::Result<Bound> {
    let listener = tokio::net::TcpListener::bind((host, port)).await?;
    let local_addr = listener.local_addr()?;
    let task = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, router).await {
            tracing::error!("{name} listener stopped: {e}");
        }
    });
    Ok(Bound { local_addr, task })
}

/// Resolves on SIGINT or SIGTERM.
///
/// Ctrl-C is what a terminal sends. SIGTERM is what `docker stop`, a process supervisor and a
/// cancelled CI job send, and to a node with state to write the two mean the same thing: stop, but
/// finish what you promised first. Waiting only on ctrl-c loses the `--state` dump and leaves
/// `hanvil run`'s agent, dev server and browser orphaned.
pub async fn interrupt() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let mut terminate = signal(SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result,
            _ = terminate.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await
    }
}
