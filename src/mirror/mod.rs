//! Mirror node REST API, in the shape `hiero-mirror-node` returns it
//! (`rest/api/v1/openapi.yml`): snake_case fields, `sec.nanos` timestamps, `0.0.N` ids, amounts
//! in tinybar, one page per list. Read-only — every mutation reaches the chain through JSON-RPC
//! or HAPI.

mod accounts;
mod blocks;
mod contracts;
mod network;
mod shapes;
mod transactions;

use std::collections::HashMap;

use axum::Router;
use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::get;
use serde_json::{Value, json};

use crate::serve::{Bound, Shared};

/// What the mirror refuses, in the wire shape `openapi.yml` gives each status.
#[derive(Debug)]
pub enum Error {
    /// 404, with the wording the spec's example carries for that resource (`openapi.yml:4522`).
    NotFound {
        /// The `message` field.
        message: String,
        /// The `detail` field, when there is something useful to add.
        detail: Option<String>,
    },
    /// 400, for a parameter that will never come right however long the caller waits — PR #39's
    /// reader stops polling on these.
    Invalid {
        /// The `message` field.
        message: String,
    },
}

impl Error {
    /// The plain 404 (`openapi.yml:4522` NotFoundError).
    pub fn not_found() -> Self {
        Self::NotFound {
            message: "Not found".to_string(),
            detail: None,
        }
    }

    /// 400 for a malformed query or path parameter (`openapi.yml:4563` InvalidParameterError).
    pub fn invalid_parameter(name: &str) -> Self {
        Self::Invalid {
            message: format!("Invalid parameter: {name}"),
        }
    }

    /// 400 for a transaction id in the SDK's `0.0.x@sss.nnn` form. The spec's example escapes the
    /// quotes around the format; the mirror sends them.
    pub fn invalid_transaction_id() -> Self {
        Self::Invalid {
            message: "Invalid Transaction id. Please use \"shard.realm.num-sss-nnn\" format where \
                      sss are seconds and nnn are nanoseconds"
                .to_string(),
        }
    }

    /// 404 for a path Hanvil does not serve, naming it so a typo is not read as an empty network.
    fn unserved(path: &str) -> Self {
        Self::NotFound {
            message: "Not found".to_string(),
            detail: Some(format!(
                "hanvil does not serve {path}; the endpoints it does serve are listed in its README"
            )),
        }
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let (status, message, detail) = match self {
            Self::NotFound { message, detail } => (StatusCode::NOT_FOUND, message, detail),
            Self::Invalid { message } => (StatusCode::BAD_REQUEST, message, None),
        };
        let mut entry = json!({ "message": message });
        if let Some(detail) = detail {
            entry["detail"] = json!(detail);
        }
        (status, Json(json!({ "_status": { "messages": [entry] } }))).into_response()
    }
}

/// Query string as the mirror spells it: `account.id`, `transactiontype`, `limit`, `order`.
pub struct Params(HashMap<String, String>);

/// Listing direction (`openapi.yml:5054`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Order {
    /// Oldest first.
    Asc,
    /// Newest first.
    Desc,
}

impl Params {
    fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(String::as_str)
    }

    /// `limit`: default 25, 1..=100 (`openapi.yml:4904`).
    fn limit(&self) -> Result<usize, Error> {
        match self.get("limit") {
            None => Ok(25),
            Some(text) => match text.parse::<usize>() {
                Ok(n) if (1..=100).contains(&n) => Ok(n),
                _ => Err(Error::invalid_parameter("limit")),
            },
        }
    }

    fn order(&self, default: Order) -> Result<Order, Error> {
        match self.get("order") {
            None => Ok(default),
            Some("asc") => Ok(Order::Asc),
            Some("desc") => Ok(Order::Desc),
            Some(_) => Err(Error::invalid_parameter("order")),
        }
    }

    fn flag(&self, name: &str, default: bool) -> Result<bool, Error> {
        match self.get(name) {
            None => Ok(default),
            Some("true") => Ok(true),
            Some("false") => Ok(false),
            Some(_) => Err(Error::invalid_parameter(name)),
        }
    }

    /// An entity-id filter such as `account.id=0.0.1002`. The mirror also accepts `gt:`/`lt:`
    /// operators; Hanvil accepts equality and refuses the rest by name.
    fn entity_filter(&self, name: &str) -> Result<Option<crate::state::EntityId>, Error> {
        let Some(text) = self.get(name) else {
            return Ok(None);
        };
        // The mirror also takes gt:/gte:/lt:/lte: here; Hanvil takes equality and says so.
        let value = text.strip_prefix("eq:").unwrap_or(text);
        if value.contains(':') {
            return Err(Error::invalid_parameter(name));
        }
        shapes::parse_entity_id(value)
            .map(Some)
            .map_err(|()| Error::invalid_parameter(name))
    }
}

/// Take `limit` items in `order` from a list held oldest first.
fn page<T>(items: Vec<T>, order: Order, limit: usize) -> Vec<T> {
    let mut items = items;
    if order == Order::Desc {
        items.reverse();
    }
    items.truncate(limit);
    items
}

/// Bind and serve. Returns once the socket is listening.
pub async fn serve(chain: Shared, host: &str, port: u16) -> std::io::Result<Bound> {
    let router = Router::new()
        .route("/api/v1/accounts/{id}", get(accounts::get))
        .route("/api/v1/accounts/{id}/tokens", get(accounts::tokens))
        .route("/api/v1/transactions", get(transactions::list))
        .route("/api/v1/transactions/{id}", get(transactions::by_id))
        .route("/api/v1/contracts/results/logs", get(contracts::logs))
        .route("/api/v1/contracts/results/{hash}", get(contracts::result))
        .route("/api/v1/contracts/{id}", get(contracts::get))
        .route("/api/v1/contracts/{id}/results", get(contracts::results))
        .route("/api/v1/contracts/{id}/results/logs", get(contracts::logs))
        .route("/api/v1/blocks", get(blocks::list))
        .route("/api/v1/blocks/{id}", get(blocks::by_id))
        .route("/api/v1/network/nodes", get(network::nodes))
        .route("/api/v1/network/exchangerate", get(network::exchange_rate))
        .route("/api/v1/network/fees", get(network::fees))
        .fallback(unserved)
        .with_state(chain);
    crate::serve::bind(host, port, router, "mirror").await
}

async fn unserved(uri: axum::http::Uri) -> Error {
    Error::unserved(uri.path())
}

impl<S: Send + Sync> axum::extract::FromRequestParts<S> for Params {
    type Rejection = Error;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        let Query(map) = Query::<HashMap<String, String>>::from_request_parts(parts, state)
            .await
            .map_err(|_| Error::invalid_parameter("query"))?;
        Ok(Self(map))
    }
}

/// A handler's answer: one JSON body, or the wire error.
pub type Answer = Result<Json<Value>, Error>;
