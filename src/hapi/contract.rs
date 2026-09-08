//! `SmartContractService` (`smart_contract_service.proto`). Two methods do work:
//! `callEthereum`, which carries the same EIP-2718 bytes `eth_sendRawTransaction` takes, and
//! `contractCallLocalMethod`, which is `eth_call`. Everything else names JSON-RPC and returns
//! `NOT_SUPPORTED` rather than pretending.

use alloy_primitives::Bytes;
use revm::context_interface::result::ExecutionResult;
use tonic::{Request, Response, Status as GrpcStatus};

use super::proto::smart_contract_service_server::SmartContractService;
use super::{Node, proto, queries, render, wire};
use crate::state::{CallRequest, Status};

/// The message every refused contract method carries, so a caller is pointed at the surface that
/// does work rather than left guessing.
const USE_JSON_RPC: &str = "hanvil does not serve this contract method over HAPI; \
                            deploy and call over JSON-RPC on :7546";

#[tonic::async_trait]
impl SmartContractService for Node {
    /// `EthereumTransaction`: the HAPI wrapper the relay uses to submit an EVM transaction.
    async fn call_ethereum(
        &self,
        request: Request<proto::Transaction>,
    ) -> Result<Response<proto::TransactionResponse>, GrpcStatus> {
        Ok(Response::new(self.submit(request.get_ref())))
    }

    /// `ContractCallQuery`: read-only execution against the head state.
    async fn contract_call_local_method(
        &self,
        request: Request<proto::Query>,
    ) -> Result<Response<proto::Response>, GrpcStatus> {
        let query = match request.get_ref().query.as_ref() {
            Some(proto::query::Query::ContractCallLocal(query)) => query,
            _ => return Err(GrpcStatus::invalid_argument("expected contractCallLocal")),
        };
        let ask = queries::ask(query.header.as_ref());
        let mut response = proto::ContractCallLocalResponse {
            header: Some(queries::answered(ask)),
            function_result: None,
        };
        if ask == queries::Ask::Answer {
            let mut chain = self.chain.write();
            let target = render::contract_entity(&chain, query.contract_id.as_ref())
                .and_then(|id| chain.contract(id))
                .map(|contract| contract.address);
            match target {
                None => response.header = Some(queries::refused(ask, Status::InvalidContractId)),
                Some(address) => {
                    let call = CallRequest {
                        from: wire::account_id(query.sender_id.as_ref())
                            .and_then(|id| chain.account(id))
                            .map(|account| account.evm_address()),
                        to: Some(address),
                        gas: Some(query.gas.max(0) as u64).filter(|gas| *gas > 0),
                        gas_price: None,
                        value: 0,
                        input: Bytes::copy_from_slice(&query.function_parameters),
                    };
                    let now = self.clock.now();
                    let contract_id = render::contract_entity(&chain, query.contract_id.as_ref())
                        .map(wire::to_contract_id);
                    let mut result = proto::ContractFunctionResult {
                        contract_id,
                        gas: query.gas,
                        function_parameters: query.function_parameters.clone(),
                        sender_id: query.sender_id.clone(),
                        ..Default::default()
                    };
                    match chain.call(&call, now) {
                        Ok(result_ok @ ExecutionResult::Success { .. }) => {
                            result.gas_used = result_ok.tx_gas_used();
                            if let ExecutionResult::Success { output, .. } = result_ok {
                                result.contract_call_result = output.into_data().to_vec();
                            }
                        }
                        // A revert is an answer, not a transport failure: the caller reads the
                        // status and the revert data, as `eth_call` returns them.
                        Ok(reverted @ ExecutionResult::Revert { .. }) => {
                            result.gas_used = reverted.tx_gas_used();
                            if let ExecutionResult::Revert { output, .. } = reverted {
                                result.contract_call_result = output.to_vec();
                            }
                            response.header =
                                Some(queries::refused(ask, Status::ContractRevertExecuted));
                        }
                        Ok(halted @ ExecutionResult::Halt { .. }) => {
                            result.gas_used = halted.tx_gas_used();
                            if let ExecutionResult::Halt { reason, .. } = halted {
                                result.error_message = format!("{reason:?}");
                            }
                            response.header =
                                Some(queries::refused(ask, Status::ContractExecutionException));
                        }
                        Err(error) => {
                            result.error_message = error.to_string();
                            response.header =
                                Some(queries::refused(ask, Status::ContractExecutionException));
                        }
                    }
                    response.function_result = Some(result);
                }
            }
        }
        Ok(queries::respond(
            proto::response::Response::ContractCallLocal(response),
        ))
    }

    async fn create_contract(
        &self,
        _: Request<proto::Transaction>,
    ) -> Result<Response<proto::TransactionResponse>, GrpcStatus> {
        Ok(Response::new(Node::unsupported()))
    }

    async fn update_contract(
        &self,
        _: Request<proto::Transaction>,
    ) -> Result<Response<proto::TransactionResponse>, GrpcStatus> {
        Ok(Response::new(Node::unsupported()))
    }

    async fn contract_call_method(
        &self,
        _: Request<proto::Transaction>,
    ) -> Result<Response<proto::TransactionResponse>, GrpcStatus> {
        Ok(Response::new(Node::unsupported()))
    }

    async fn delete_contract(
        &self,
        _: Request<proto::Transaction>,
    ) -> Result<Response<proto::TransactionResponse>, GrpcStatus> {
        Ok(Response::new(Node::unsupported()))
    }

    async fn system_delete(
        &self,
        _: Request<proto::Transaction>,
    ) -> Result<Response<proto::TransactionResponse>, GrpcStatus> {
        Ok(Response::new(Node::unsupported()))
    }

    async fn system_undelete(
        &self,
        _: Request<proto::Transaction>,
    ) -> Result<Response<proto::TransactionResponse>, GrpcStatus> {
        Ok(Response::new(Node::unsupported()))
    }

    async fn hook_store(
        &self,
        _: Request<proto::Transaction>,
    ) -> Result<Response<proto::TransactionResponse>, GrpcStatus> {
        Ok(Response::new(Node::unsupported()))
    }

    async fn get_contract_info(
        &self,
        _: Request<proto::Query>,
    ) -> Result<Response<proto::Response>, GrpcStatus> {
        Err(Node::unsupported_query(USE_JSON_RPC))
    }

    async fn contract_get_bytecode(
        &self,
        _: Request<proto::Query>,
    ) -> Result<Response<proto::Response>, GrpcStatus> {
        Err(Node::unsupported_query(USE_JSON_RPC))
    }

    async fn get_by_solidity_id(
        &self,
        _: Request<proto::Query>,
    ) -> Result<Response<proto::Response>, GrpcStatus> {
        Err(Node::unsupported_query(USE_JSON_RPC))
    }

    async fn get_tx_record_by_contract_id(
        &self,
        _: Request<proto::Query>,
    ) -> Result<Response<proto::Response>, GrpcStatus> {
        Err(Node::unsupported_query(USE_JSON_RPC))
    }
}
