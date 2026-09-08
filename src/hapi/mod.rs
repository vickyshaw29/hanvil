//! HAPI gRPC on :50211, plaintext h2c, node account 0.0.3 — the shape `Client.forLocalNode()`
//! expects (`hiero-sdk-js/src/constants/ClientConstants.js:92`).
//!
//! Four services are registered: `CryptoService`, `ConsensusService`, `SmartContractService` and
//! `NetworkService`. A method Hanvil does not emulate answers `NOT_SUPPORTED` with a message
//! naming what to use instead; it never returns a default that reads like success.

mod consensus;
mod contract;
mod crypto;
mod network;
mod queries;
mod render;
mod wire;

use std::sync::Arc;

/// Generated from proto/ by build.rs.
#[allow(
    clippy::all,
    clippy::pedantic,
    dead_code,
    missing_docs,
    unused_imports,
    unused_qualifications,
    rustdoc::bare_urls,
    rustdoc::broken_intra_doc_links,
    rustdoc::invalid_html_tags
)]
mod generated {
    include!(concat!(env!("OUT_DIR"), "/hapi.rs"));
}

/// The HAPI messages, nested under their protobuf package.
pub use generated::proto;

use proto::consensus_service_server::ConsensusServiceServer;
use proto::crypto_service_server::CryptoServiceServer;
use proto::network_service_server::NetworkServiceServer;
use proto::smart_contract_service_server::SmartContractServiceServer;

use crate::serve::{Bound, Shared};
use crate::state::{Clock, Record, Status};

/// The one consensus node this network runs. Every handler reaches the same chain the JSON-RPC
/// and mirror listeners read.
#[derive(Clone)]
pub struct Node {
    /// The chain.
    pub chain: Shared,
    /// Source of the current time.
    pub clock: Arc<dyn Clock>,
    /// `--no-sig-verify` turns this off, which lets a client submit a body it cannot sign.
    pub verify_signatures: bool,
}

impl Node {
    /// Decode, precheck and apply one transaction, then answer with the precheck code. The
    /// record is available to `getTransactionReceipts` the moment this returns, so the SDK's
    /// first poll already sees the outcome.
    fn submit(&self, envelope: &proto::Transaction) -> proto::TransactionResponse {
        let now = self.clock.now();
        let outcome = {
            let mut chain = self.chain.write();
            wire::submit(&mut chain, envelope, now, self.verify_signatures)
        };
        match outcome {
            Ok(record) => {
                log(&record);
                proto::TransactionResponse {
                    node_transaction_precheck_code: Status::Ok.code(),
                    cost: 0,
                }
            }
            Err(status) => {
                tracing::info!("hapi rejected: {}", status.name());
                proto::TransactionResponse {
                    node_transaction_precheck_code: status.code(),
                    cost: 0,
                }
            }
        }
    }

    /// The answer to a transaction Hanvil does not emulate.
    fn unsupported() -> proto::TransactionResponse {
        proto::TransactionResponse {
            node_transaction_precheck_code: Status::NotSupported.code(),
            cost: 0,
        }
    }

    /// The answer to a query Hanvil does not emulate. A `Query` has no field for a precheck code
    /// outside its own response variant, so the refusal is a gRPC status naming the method.
    fn unsupported_query(what: &str) -> tonic::Status {
        tonic::Status::unimplemented(format!("hanvil does not serve {what}"))
    }
}

/// One line per HAPI transaction, in the shape the JSON-RPC side already logs.
fn log(record: &Record) {
    tracing::info!(
        "{} {} payer {} fee {} {}",
        record.kind.name(),
        record.id,
        record.id.payer,
        record.charged_fee.0,
        record.status.name(),
    );
}

/// Bind and serve the four services. Returns once the socket is listening.
pub async fn serve(node: Node, host: &str, port: u16) -> std::io::Result<Bound> {
    let router = tonic::service::Routes::builder()
        .routes()
        .add_service(CryptoServiceServer::new(node.clone()))
        .add_service(ConsensusServiceServer::new(node.clone()))
        .add_service(SmartContractServiceServer::new(node.clone()))
        .add_service(NetworkServiceServer::new(node))
        .prepare()
        .into_axum_router();
    crate::serve::bind(host, port, router, "hapi").await
}
