//! `NetworkService` (`network_service.proto`). `getVersionInfo` is what a client pings to see
//! whether a node is up.

use tonic::{Request, Response, Status as GrpcStatus};

use super::proto::network_service_server::NetworkService;
use super::{Node, proto, queries};

/// The HAPI version the vendored protobufs came from. `services` reports Hanvil's own version so
/// a client that logs it does not think it is talking to a consensus node.
const HAPI_VERSION: (i32, i32, i32) = (0, 66, 0);

#[tonic::async_trait]
impl NetworkService for Node {
    async fn get_version_info(
        &self,
        request: Request<proto::Query>,
    ) -> Result<Response<proto::Response>, GrpcStatus> {
        let header = match request.get_ref().query.as_ref() {
            Some(proto::query::Query::NetworkGetVersionInfo(query)) => query.header.as_ref(),
            _ => {
                return Err(GrpcStatus::invalid_argument(
                    "expected networkGetVersionInfo",
                ));
            }
        };
        let ask = queries::ask(header);
        let (major, minor, patch) = HAPI_VERSION;
        Ok(queries::respond(
            proto::response::Response::NetworkGetVersionInfo(
                proto::NetworkGetVersionInfoResponse {
                    header: Some(queries::answered(ask)),
                    hapi_proto_version: Some(proto::SemanticVersion {
                        major,
                        minor,
                        patch,
                        ..Default::default()
                    }),
                    hedera_services_version: Some(proto::SemanticVersion {
                        major: 0,
                        minor: 1,
                        patch: 0,
                        pre: String::new(),
                        build: format!("hanvil-{}", env!("CARGO_PKG_VERSION")),
                    }),
                },
            ),
        ))
    }

    async fn get_account_details(
        &self,
        _: Request<proto::Query>,
    ) -> Result<Response<proto::Response>, GrpcStatus> {
        Err(Node::unsupported_query(
            "getAccountDetails; use cryptoGetInfo or /api/v1/accounts/{id} on the mirror",
        ))
    }

    async fn get_execution_time(
        &self,
        _: Request<proto::Query>,
    ) -> Result<Response<proto::Response>, GrpcStatus> {
        Err(Node::unsupported_query("networkGetExecutionTime"))
    }

    async fn unchecked_submit(
        &self,
        _: Request<proto::Transaction>,
    ) -> Result<Response<proto::TransactionResponse>, GrpcStatus> {
        Ok(Response::new(Node::unsupported()))
    }
}
