//! Query headers.
//!
//! The SDK asks a paid query twice: a `COST_ANSWER` round trip, then an `ANSWER_ONLY` request
//! carrying a `CryptoTransfer` for exactly that cost, even when the cost is zero
//! (`hiero-sdk-js/src/query/Query.js:295-350`). Hanvil answers every cost with 0 and does not
//! read the payment, so both halves of that flow work without special casing.

use super::proto;
use crate::state::Status;

/// What the caller asked for.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Ask {
    /// Answer the query.
    Answer,
    /// Only report what the answer would cost.
    Cost,
}

/// Read the response type out of a query header. A state-proof request is answered as its plain
/// counterpart; Hanvil has no state proofs and says so in the README.
pub fn ask(header: Option<&proto::QueryHeader>) -> Ask {
    match header.map(|header| header.response_type) {
        Some(2) | Some(3) => Ask::Cost,
        _ => Ask::Answer,
    }
}

/// The header on a successful answer.
pub fn answered(ask: Ask) -> proto::ResponseHeader {
    proto::ResponseHeader {
        node_transaction_precheck_code: Status::Ok.code(),
        response_type: response_type(ask),
        cost: 0,
        state_proof: Vec::new(),
    }
}

/// The header on a refusal. The SDK reads this code and stops, or retries when it is one of the
/// codes `TransactionReceiptQuery` treats as "not yet"
/// (`hiero-sdk-js/src/transaction/TransactionReceiptQuery.js:209-214`).
pub fn refused(ask: Ask, status: Status) -> proto::ResponseHeader {
    proto::ResponseHeader {
        node_transaction_precheck_code: status.code(),
        response_type: response_type(ask),
        cost: 0,
        state_proof: Vec::new(),
    }
}

fn response_type(ask: Ask) -> i32 {
    match ask {
        Ask::Answer => proto::ResponseType::AnswerOnly as i32,
        Ask::Cost => proto::ResponseType::CostAnswer as i32,
    }
}

/// Wrap a response variant in the `Response` envelope.
pub fn respond(response: proto::response::Response) -> tonic::Response<proto::Response> {
    tonic::Response::new(proto::Response {
        response: Some(response),
    })
}
