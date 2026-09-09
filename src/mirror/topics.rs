//! `/api/v1/topics/{id}` and `/api/v1/topics/{id}/messages`, filled in by
//! `consensusCreateTopic` and `consensusSubmitMessage` over HAPI.

use axum::extract::{Path, State};
use axum::response::Json;
use serde_json::{Value, json};

use super::shapes::{self, timestamp, timestamp_range};
use super::{Answer, Error, Order, Params, page};
use crate::serve::Shared;
use crate::state::{EntityId, Topic, TopicMessage, hapi};

/// `GET /api/v1/topics/{topicId}` — `openapi.yml:4138` Topic.
pub async fn get(State(chain): State<Shared>, Path(id): Path<String>, params: Params) -> Answer {
    params.only(&[])?;
    let id = shapes::parse_entity_id(&id).map_err(|()| Error::invalid_parameter("topicId"))?;
    let chain = chain.read();
    let topic = chain.topic(id).ok_or_else(|| not_found(id))?;
    Ok(Json(topic_body(topic)))
}

/// `GET /api/v1/topics/{topicId}/messages` — `openapi.yml:1140`. `sequencenumber` selects one
/// message; `limit` and `order` page the rest.
pub async fn messages(
    State(chain): State<Shared>,
    Path(id): Path<String>,
    params: Params,
) -> Answer {
    let id = shapes::parse_entity_id(&id).map_err(|()| Error::invalid_parameter("topicId"))?;
    params.only(&["limit", "order", "sequencenumber"])?;
    let limit = params.limit()?;
    let order = params.order(Order::Asc)?;
    let wanted = match params.get("sequencenumber") {
        None => None,
        Some(text) => Some(
            text.parse::<u64>()
                .map_err(|_| Error::invalid_parameter("sequencenumber"))?,
        ),
    };

    let chain = chain.read();
    let topic = chain.topic(id).ok_or_else(|| not_found(id))?;
    let matched: Vec<Value> = topic
        .messages
        .iter()
        .filter(|message| wanted.is_none_or(|n| n == message.sequence_number))
        .map(|message| message_body(id, message))
        .collect();
    Ok(Json(json!({
        "messages": page(matched, order, limit),
        "links": shapes::links(),
    })))
}

/// `GET /api/v1/topics/{topicId}/messages/{sequenceNumber}` — `openapi.yml:1175`.
pub async fn message(
    State(chain): State<Shared>,
    Path((id, sequence_number)): Path<(String, String)>,
    params: Params,
) -> Answer {
    params.only(&[])?;
    let id = shapes::parse_entity_id(&id).map_err(|()| Error::invalid_parameter("topicId"))?;
    let sequence_number: u64 = sequence_number
        .parse()
        .map_err(|_| Error::invalid_parameter("sequenceNumber"))?;
    let chain = chain.read();
    let topic = chain.topic(id).ok_or_else(|| not_found(id))?;
    let found = topic
        .messages
        .iter()
        .find(|message| message.sequence_number == sequence_number)
        .ok_or_else(Error::not_found)?;
    Ok(Json(message_body(id, found)))
}

/// `openapi.yml:4543` TopicNotFound: the message names the topic number, not the whole id.
fn not_found(id: EntityId) -> Error {
    Error::NotFound {
        message: format!("No such topic id - {}", id.0),
        detail: None,
    }
}

/// `openapi.yml:4138` Topic. Every required field is present; the custom-fee fields are the empty
/// shapes a topic without custom fees carries.
fn topic_body(topic: &Topic) -> Value {
    json!({
        "admin_key": shapes::key(topic.admin_key.as_ref()),
        "auto_renew_account": topic
            .auto_renew_account
            .map_or(Value::Null, |id| json!(id.to_string())),
        "auto_renew_period": topic.auto_renew_period,
        "created_timestamp": timestamp(topic.created_at),
        "custom_fees": { "created_timestamp": timestamp(topic.created_at), "fixed_fees": [] },
        "deleted": false,
        "fee_exempt_key_list": [],
        "fee_schedule_key": Value::Null,
        "memo": topic.memo,
        "submit_key": shapes::key(topic.submit_key.as_ref()),
        "timestamp": timestamp_range(topic.created_at, None),
        "topic_id": topic.id.to_string(),
    })
}

/// `openapi.yml:4188` TopicMessage. `message` and `running_hash` are base64 (`format: byte`).
fn message_body(topic: EntityId, message: &TopicMessage) -> Value {
    json!({
        // Hanvil stores each submitted chunk as its own message and keeps no chunk metadata
        // (README, what is not emulated), so this optional field is null rather than invented.
        "chunk_info": Value::Null,
        "consensus_timestamp": timestamp(message.consensus_timestamp),
        "message": shapes::base64(&message.message),
        "payer_account_id": message.payer.to_string(),
        "running_hash": shapes::base64(message.running_hash.as_bytes()),
        "running_hash_version": hapi::RUNNING_HASH_VERSION,
        "sequence_number": message.sequence_number,
        "topic_id": topic.to_string(),
    })
}
