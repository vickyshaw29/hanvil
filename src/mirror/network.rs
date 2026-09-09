//! `/api/v1/network/...`. One node, a fixed exchange rate, and the gas price the chain charges.

use axum::extract::State;
use axum::response::Json;
use serde_json::{Value, json};

use super::shapes::{self, timestamp, timestamp_range};
use super::{Answer, Params};
use crate::serve::Shared;
use crate::state::{CENT_EQUIVALENT, EXCHANGE_RATE_VALID_SECS, HBAR_EQUIVALENT, NODE};

/// `GET /api/v1/network/nodes` — `openapi.yml:2982` NetworkNode. Hanvil is one node, 0.0.3, the
/// account every HAPI transaction must name.
pub async fn nodes(State(chain): State<Shared>, params: Params) -> Answer {
    params.only(&[])?;
    let chain = chain.read();
    let genesis = chain.genesis_timestamp();
    Ok(Json(json!({
        "nodes": [{
            "admin_key": Value::Null,
            "associated_registered_nodes": [],
            "decline_reward": false,
            "description": "hanvil",
            "file_id": "0.0.102",
            // VERIFY: the schema types this as a ServiceEndpoint and requires it; a real mirror
            // sends null when the node publishes no proxy. Hanvil publishes none.
            "grpc_proxy_endpoint": Value::Null,
            "max_stake": Value::Null,
            "memo": NODE.to_string(),
            "min_stake": Value::Null,
            "node_account_id": NODE.to_string(),
            "node_id": 0,
            "node_cert_hash": Value::Null,
            "public_key": Value::Null,
            "reward_rate_start": 0,
            "service_endpoints": [],
            "stake": 0,
            "stake_not_rewarded": 0,
            "stake_rewarded": 0,
            "staking_period": Value::Null,
            "timestamp": timestamp_range(genesis, None),
        }],
        "links": shapes::links(),
    })))
}

/// `GET /api/v1/network/exchangerate` — `openapi.yml:2865` ExchangeRate.
///
/// VERIFY: 1 ℏ = 12 ¢ is Hedera's long-standing default rate; the value hiero-local-node serves
/// was not found in its sources (docs/research.md §6, 2026-09-07).
pub async fn exchange_rate(State(chain): State<Shared>, params: Params) -> Answer {
    params.only(&[])?;
    let chain = chain.read();
    let now = chain.latest_block().consensus_timestamp;
    let rate = json!({
        "cent_equivalent": CENT_EQUIVALENT,
        "expiration_time": now.secs + EXCHANGE_RATE_VALID_SECS,
        "hbar_equivalent": HBAR_EQUIVALENT,
    });
    Ok(Json(json!({
        "current_rate": rate,
        "next_rate": rate,
        "timestamp": timestamp(now),
    })))
}

/// `GET /api/v1/network/fees` — `openapi.yml:3108` NetworkFee. The gas price the chain actually
/// charges, in tinybar, for the three types the mirror reports.
pub async fn fees(State(chain): State<Shared>, params: Params) -> Answer {
    use super::Order;

    params.only(&["order"])?;
    let order = params.order(Order::Asc)?;
    let chain = chain.read();
    let gas = chain.gas_price().0;
    let mut fees: Vec<Value> = ["ContractCall", "ContractCreate", "EthereumTransaction"]
        .into_iter()
        .map(|kind| json!({ "gas": gas, "transaction_type": kind }))
        .collect();
    if order == Order::Desc {
        fees.reverse();
    }
    Ok(Json(json!({
        "fees": fees,
        "timestamp": timestamp(chain.latest_block().consensus_timestamp),
    })))
}
