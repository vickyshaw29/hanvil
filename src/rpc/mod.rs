//! JSON-RPC listener in the shape of the Hedera JSON-RPC relay, plus Anvil's cheats.

mod cheats;
mod eth;
mod types;

use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::post;
use serde_json::{Value, json};

use crate::evm::Rejected;
use crate::serve::{Bound, Shared};
use crate::state::{self, Clock};

/// What a handler needs: the chain and the time.
#[derive(Clone)]
pub struct App {
    /// The one chain.
    pub chain: Shared,
    /// Source of "now" for new blocks.
    pub clock: Arc<dyn Clock>,
}

/// JSON-RPC error, serialised exactly as the relay does.
#[derive(Debug, Clone)]
pub struct RpcError {
    code: i64,
    message: String,
    data: Option<Value>,
}

impl RpcError {
    /// -32700
    pub fn parse() -> Self {
        Self::new(-32700, "Parse error")
    }
    /// -32600
    pub fn invalid_request() -> Self {
        Self::new(-32600, "Invalid Request")
    }
    /// -32601, the relay's wording for a method it knows and refuses.
    pub fn unsupported(method: &str) -> Self {
        Self::new(-32601, format!("Method {method} not supported"))
    }
    /// -32601 with the reason and the alternative.
    pub fn unsupported_with_reason(method: &str, reason: &str) -> Self {
        Self::new(-32601, format!("Method {method} not supported: {reason}"))
    }
    /// -32601 for a method that does not exist.
    pub fn method_not_found(method: &str) -> Self {
        Self::new(-32601, format!("Method {method} not found"))
    }
    /// -32602
    pub fn invalid_params(detail: impl Into<String>) -> Self {
        Self::new(-32602, format!("Invalid params: {}", detail.into()))
    }
    /// -32000, the generic server error the relay uses for rejected transactions.
    pub fn server(detail: impl Into<String>) -> Self {
        Self::new(-32000, detail.into())
    }
    /// Code 3 with revert data, the geth shape viem and ethers decode custom errors from.
    pub fn execution_reverted(message: String, revert_data: String) -> Self {
        Self {
            code: 3,
            message,
            data: Some(Value::String(revert_data)),
        }
    }

    fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    fn to_json(&self) -> Value {
        let mut err = json!({ "code": self.code, "message": self.message });
        if let Some(data) = &self.data {
            err["data"] = data.clone();
        }
        err
    }
}

impl From<state::Error> for RpcError {
    fn from(error: state::Error) -> Self {
        match error {
            state::Error::Decode(e) => Self::invalid_params(e.to_string()),
            state::Error::Rejected(Rejected::Other(text)) => Self::server(text),
            state::Error::Rejected(rejected) => Self::server(rejected.to_string()),
            other => Self::server(other.to_string()),
        }
    }
}

/// Bind and serve. Returns once the socket is listening.
pub async fn serve(app: App, host: &str, port: u16) -> std::io::Result<Bound> {
    let router = Router::new().route("/", post(handle)).with_state(app);
    crate::serve::bind(host, port, router, "json-rpc").await
}

async fn handle(State(app): State<App>, body: Bytes) -> Response {
    let parsed: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return Json(error_response(Value::Null, RpcError::parse())).into_response(),
    };
    let out = match parsed {
        Value::Array(batch) => Value::Array(batch.iter().map(|req| dispatch(&app, req)).collect()),
        single => dispatch(&app, &single),
    };
    Json(out).into_response()
}

fn dispatch(app: &App, request: &Value) -> Value {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let Some(method) = request.get("method").and_then(Value::as_str) else {
        return error_response(id, RpcError::invalid_request());
    };
    let params: Vec<Value> = match request.get("params") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(a)) => a.clone(),
        Some(_) => return error_response(id, RpcError::invalid_params("params must be an array")),
    };
    let now = app.clock.now();
    let result = {
        let mut chain = app.chain.write();
        if cheats::handles(method) {
            cheats::call(&mut chain, now, method, &params)
        } else {
            eth::call(&mut chain, now, method, &params)
        }
    };
    match result {
        Ok(value) => json!({ "jsonrpc": "2.0", "id": id, "result": value }),
        Err(e) => {
            tracing::debug!(method, code = e.code, "{}", e.message);
            error_response(id, e)
        }
    }
}

fn error_response(id: Value, error: RpcError) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": error.to_json() })
}
