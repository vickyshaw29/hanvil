//! JSON-RPC listener in the shape of the Hedera JSON-RPC relay.

mod eth;
mod types;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::post;
use parking_lot::RwLock;
use serde_json::{Value, json};
use tokio::task::JoinHandle;

use crate::state::Chain;

/// The chain shared by all listeners.
pub type Shared = Arc<RwLock<Chain>>;

/// A running listener.
pub struct Bound {
    /// Address actually bound (matters when the port was 0).
    pub local_addr: SocketAddr,
    /// The serving task.
    pub task: JoinHandle<()>,
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
    /// -32601 for a method that does not exist.
    pub fn method_not_found(method: &str) -> Self {
        Self::new(-32601, format!("Method {method} not found"))
    }
    /// -32602
    pub fn invalid_params(detail: impl Into<String>) -> Self {
        Self::new(-32602, format!("Invalid params: {}", detail.into()))
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

/// Bind and serve. Returns once the socket is listening.
pub async fn serve(chain: Shared, host: &str, port: u16) -> std::io::Result<Bound> {
    let listener = tokio::net::TcpListener::bind((host, port)).await?;
    let local_addr = listener.local_addr()?;
    let app = Router::new().route("/", post(handle)).with_state(chain);
    let task = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!("json-rpc listener stopped: {e}");
        }
    });
    Ok(Bound { local_addr, task })
}

async fn handle(State(chain): State<Shared>, body: Bytes) -> Response {
    let parsed: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return Json(error_response(Value::Null, RpcError::parse())).into_response(),
    };
    let out = match parsed {
        Value::Array(batch) => {
            Value::Array(batch.iter().map(|req| dispatch(&chain, req)).collect())
        }
        single => dispatch(&chain, &single),
    };
    Json(out).into_response()
}

fn dispatch(chain: &Shared, request: &Value) -> Value {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let Some(method) = request.get("method").and_then(Value::as_str) else {
        return error_response(id, RpcError::invalid_request());
    };
    let params: Vec<Value> = match request.get("params") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(a)) => a.clone(),
        Some(_) => return error_response(id, RpcError::invalid_params("params must be an array")),
    };
    let result = {
        let guard = chain.read();
        eth::call(&guard, method, &params)
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
