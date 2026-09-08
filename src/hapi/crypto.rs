//! `CryptoService` (`crypto_service.proto`): the transactions and queries
//! `@hiero-ledger/sdk`'s account classes issue.

use tonic::{Request, Response, Status as GrpcStatus};

use super::proto::crypto_service_server::CryptoService;
use super::{Node, proto, queries, render, wire};
use crate::state::Status;

#[tonic::async_trait]
impl CryptoService for Node {
    async fn create_account(
        &self,
        request: Request<proto::Transaction>,
    ) -> Result<Response<proto::TransactionResponse>, GrpcStatus> {
        Ok(Response::new(self.submit(request.get_ref())))
    }

    async fn crypto_transfer(
        &self,
        request: Request<proto::Transaction>,
    ) -> Result<Response<proto::TransactionResponse>, GrpcStatus> {
        Ok(Response::new(self.submit(request.get_ref())))
    }

    async fn crypto_delete(
        &self,
        request: Request<proto::Transaction>,
    ) -> Result<Response<proto::TransactionResponse>, GrpcStatus> {
        Ok(Response::new(self.submit(request.get_ref())))
    }

    async fn update_account(
        &self,
        _: Request<proto::Transaction>,
    ) -> Result<Response<proto::TransactionResponse>, GrpcStatus> {
        Ok(Response::new(Node::unsupported()))
    }

    async fn approve_allowances(
        &self,
        _: Request<proto::Transaction>,
    ) -> Result<Response<proto::TransactionResponse>, GrpcStatus> {
        Ok(Response::new(Node::unsupported()))
    }

    async fn delete_allowances(
        &self,
        _: Request<proto::Transaction>,
    ) -> Result<Response<proto::TransactionResponse>, GrpcStatus> {
        Ok(Response::new(Node::unsupported()))
    }

    async fn add_live_hash(
        &self,
        _: Request<proto::Transaction>,
    ) -> Result<Response<proto::TransactionResponse>, GrpcStatus> {
        Ok(Response::new(Node::unsupported()))
    }

    async fn delete_live_hash(
        &self,
        _: Request<proto::Transaction>,
    ) -> Result<Response<proto::TransactionResponse>, GrpcStatus> {
        Ok(Response::new(Node::unsupported()))
    }

    async fn get_live_hash(
        &self,
        _: Request<proto::Query>,
    ) -> Result<Response<proto::Response>, GrpcStatus> {
        Err(Node::unsupported_query("cryptoGetLiveHash"))
    }

    async fn get_account_records(
        &self,
        _: Request<proto::Query>,
    ) -> Result<Response<proto::Response>, GrpcStatus> {
        Err(Node::unsupported_query(
            "cryptoGetAccountRecords; read /api/v1/transactions?account.id= on the mirror instead",
        ))
    }

    /// `AccountBalanceQuery`. Free, so the SDK sends `ANSWER_ONLY` with no payment.
    async fn crypto_get_balance(
        &self,
        request: Request<proto::Query>,
    ) -> Result<Response<proto::Response>, GrpcStatus> {
        use proto::crypto_get_account_balance_query::BalanceSource;

        let query = match request.get_ref().query.as_ref() {
            Some(proto::query::Query::CryptogetAccountBalance(query)) => query,
            _ => {
                return Err(GrpcStatus::invalid_argument(
                    "expected cryptogetAccountBalance",
                ));
            }
        };
        let ask = queries::ask(query.header.as_ref());
        if ask == queries::Ask::Cost {
            return Ok(queries::respond(
                proto::response::Response::CryptogetAccountBalance(
                    proto::CryptoGetAccountBalanceResponse {
                        header: Some(queries::answered(ask)),
                        ..Default::default()
                    },
                ),
            ));
        }

        let chain = self.chain.read();
        let found = match query.balance_source.as_ref() {
            Some(BalanceSource::AccountId(id)) => wire::account_id(Some(id))
                .and_then(|id| chain.account(id))
                .map(|account| (account.id, account.balance.0)),
            Some(BalanceSource::ContractId(id)) => {
                render::contract_entity(&chain, Some(id)).map(|id| {
                    let address = chain
                        .contract(id)
                        .map(|contract| contract.address)
                        .unwrap_or_default();
                    (id, chain.balance_by_evm(&address).0)
                })
            }
            None => None,
        };
        let Some((id, balance)) = found else {
            return Ok(queries::respond(
                proto::response::Response::CryptogetAccountBalance(
                    proto::CryptoGetAccountBalanceResponse {
                        header: Some(queries::refused(ask, Status::InvalidAccountId)),
                        ..Default::default()
                    },
                ),
            ));
        };
        Ok(queries::respond(
            proto::response::Response::CryptogetAccountBalance(
                proto::CryptoGetAccountBalanceResponse {
                    header: Some(queries::answered(ask)),
                    account_id: Some(wire::to_account_id(id)),
                    balance,
                    ..Default::default()
                },
            ),
        ))
    }

    /// `AccountInfoQuery`. Paid, so the SDK asks the cost first and then attaches a payment for
    /// it; the payment is decoded by neither side because the cost is zero.
    async fn get_account_info(
        &self,
        request: Request<proto::Query>,
    ) -> Result<Response<proto::Response>, GrpcStatus> {
        let query = match request.get_ref().query.as_ref() {
            Some(proto::query::Query::CryptoGetInfo(query)) => query,
            _ => return Err(GrpcStatus::invalid_argument("expected cryptoGetInfo")),
        };
        let ask = queries::ask(query.header.as_ref());
        let mut response = proto::CryptoGetInfoResponse {
            header: Some(queries::answered(ask)),
            account_info: None,
        };
        if ask == queries::Ask::Answer {
            let chain = self.chain.read();
            match wire::account_id(query.account_id.as_ref()).and_then(|id| chain.account(id)) {
                Some(account) => response.account_info = Some(render::account_info(account)),
                None => response.header = Some(queries::refused(ask, Status::InvalidAccountId)),
            }
        }
        Ok(queries::respond(proto::response::Response::CryptoGetInfo(
            response,
        )))
    }

    /// `TransactionReceiptQuery`. Free. `RECEIPT_NOT_FOUND` is one of the codes the SDK retries
    /// on, so a client that polls before consensus keeps polling
    /// (`hiero-sdk-js/src/transaction/TransactionReceiptQuery.js:209-214`).
    async fn get_transaction_receipts(
        &self,
        request: Request<proto::Query>,
    ) -> Result<Response<proto::Response>, GrpcStatus> {
        let query = match request.get_ref().query.as_ref() {
            Some(proto::query::Query::TransactionGetReceipt(query)) => query,
            _ => {
                return Err(GrpcStatus::invalid_argument(
                    "expected transactionGetReceipt",
                ));
            }
        };
        let ask = queries::ask(query.header.as_ref());
        let mut response = proto::TransactionGetReceiptResponse {
            header: Some(queries::answered(ask)),
            ..Default::default()
        };
        if ask == queries::Ask::Answer {
            let chain = self.chain.read();
            match wire::transaction_id(query.transaction_id.as_ref())
                .and_then(|id| chain.hapi_record(&id))
            {
                Some(record) => response.receipt = Some(render::receipt(&chain, record)),
                None => response.header = Some(queries::refused(ask, render::NOT_FOUND)),
            }
        }
        Ok(queries::respond(
            proto::response::Response::TransactionGetReceipt(response),
        ))
    }

    /// `TransactionRecordQuery`. Paid, and answered from the same record the receipt comes from.
    async fn get_tx_record_by_tx_id(
        &self,
        request: Request<proto::Query>,
    ) -> Result<Response<proto::Response>, GrpcStatus> {
        let query = match request.get_ref().query.as_ref() {
            Some(proto::query::Query::TransactionGetRecord(query)) => query,
            _ => {
                return Err(GrpcStatus::invalid_argument(
                    "expected transactionGetRecord",
                ));
            }
        };
        let ask = queries::ask(query.header.as_ref());
        let mut response = proto::TransactionGetRecordResponse {
            header: Some(queries::answered(ask)),
            ..Default::default()
        };
        if ask == queries::Ask::Answer {
            let chain = self.chain.read();
            match wire::transaction_id(query.transaction_id.as_ref())
                .and_then(|id| chain.hapi_record(&id))
            {
                Some(record) => {
                    response.transaction_record = Some(render::transaction_record(&chain, record));
                }
                None => response.header = Some(queries::refused(ask, Status::RecordNotFound)),
            }
        }
        Ok(queries::respond(
            proto::response::Response::TransactionGetRecord(response),
        ))
    }
}
