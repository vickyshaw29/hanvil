//! `ConsensusService` (`consensus_service.proto`): HCS topics, which is what the x402 PRD's
//! receipt log uses.

use tonic::{Request, Response, Status as GrpcStatus};

use super::proto::consensus_service_server::ConsensusService;
use super::{Node, proto, queries, render, wire};
use crate::state::Status;

#[tonic::async_trait]
impl ConsensusService for Node {
    async fn create_topic(
        &self,
        request: Request<proto::Transaction>,
    ) -> Result<Response<proto::TransactionResponse>, GrpcStatus> {
        Ok(Response::new(self.submit(request.get_ref())))
    }

    async fn submit_message(
        &self,
        request: Request<proto::Transaction>,
    ) -> Result<Response<proto::TransactionResponse>, GrpcStatus> {
        Ok(Response::new(self.submit(request.get_ref())))
    }

    async fn update_topic(
        &self,
        _: Request<proto::Transaction>,
    ) -> Result<Response<proto::TransactionResponse>, GrpcStatus> {
        Ok(Response::new(Node::unsupported()))
    }

    async fn delete_topic(
        &self,
        _: Request<proto::Transaction>,
    ) -> Result<Response<proto::TransactionResponse>, GrpcStatus> {
        Ok(Response::new(Node::unsupported()))
    }

    /// `TopicInfoQuery`. Paid, like every non-receipt query.
    async fn get_topic_info(
        &self,
        request: Request<proto::Query>,
    ) -> Result<Response<proto::Response>, GrpcStatus> {
        let query = match request.get_ref().query.as_ref() {
            Some(proto::query::Query::ConsensusGetTopicInfo(query)) => query,
            _ => {
                return Err(GrpcStatus::invalid_argument(
                    "expected consensusGetTopicInfo",
                ));
            }
        };
        let ask = queries::ask(query.header.as_ref());
        let mut response = proto::ConsensusGetTopicInfoResponse {
            header: Some(queries::answered(ask)),
            topic_id: query.topic_id,
            topic_info: None,
        };
        if ask == queries::Ask::Answer {
            let chain = self.chain.read();
            match wire::topic_id(query.topic_id.as_ref()).and_then(|id| chain.topic(id)) {
                Some(topic) => response.topic_info = Some(render::topic_info(topic)),
                None => response.header = Some(queries::refused(ask, Status::InvalidTopicId)),
            }
        }
        Ok(queries::respond(
            proto::response::Response::ConsensusGetTopicInfo(response),
        ))
    }
}
