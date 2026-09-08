//! `/api/v1/blocks` and `/api/v1/blocks/{hashOrNumber}`.
//!
//! On Hedera a block is a record file, so the mirror names it after one and hashes it with
//! SHA-384. Hanvil mines one block per transaction and hashes with keccak; the fields that would
//! be a guess (`hapi_version`) are null rather than invented.

use alloy_primitives::B256;
use axum::extract::{Path, State};
use axum::response::Json;
use serde_json::{Value, json};

use super::shapes::{self, timestamp_range};
use super::{Answer, Error, Order, Params, page};
use crate::serve::Shared;
use crate::state::{Block, Chain, TxBody};

/// `GET /api/v1/blocks`.
pub async fn list(State(chain): State<Shared>, params: Params) -> Answer {
    let limit = params.limit()?;
    let order = params.order(Order::Desc)?;
    let chain = chain.read();
    let all: Vec<Value> = chain
        .blocks()
        .iter()
        .map(|b| block_body(&chain, b))
        .collect();
    Ok(Json(json!({
        "blocks": page(all, order, limit),
        "links": shapes::links(),
    })))
}

/// `GET /api/v1/blocks/{hashOrNumber}`.
pub async fn by_id(State(chain): State<Shared>, Path(id): Path<String>) -> Answer {
    let chain = chain.read();
    let found = match id.strip_prefix("0x") {
        Some(body) => hex::decode(body)
            .ok()
            .filter(|bytes| bytes.len() == 32)
            .map(|bytes| B256::from_slice(&bytes))
            .ok_or_else(|| Error::invalid_parameter("hashOrNumber"))
            .map(|hash| chain.block_by_hash(&hash))?,
        None => {
            let number = id
                .parse::<u64>()
                .map_err(|_| Error::invalid_parameter("hashOrNumber"))?;
            chain.block_by_number(number)
        }
    };
    let block = found.ok_or_else(Error::not_found)?;
    Ok(Json(block_body(&chain, block)))
}

/// `openapi.yml:3336` Block.
fn block_body(chain: &Chain, block: &Block) -> Value {
    let last = block
        .transactions
        .last()
        .and_then(|hash| chain.transaction(hash))
        .map_or(block.consensus_timestamp, |tx| tx.consensus_timestamp);
    json!({
        "count": block.transactions.len(),
        "gas_used": block.gas_used,
        // Hanvil is not a consensus node and does not claim one of its versions.
        "hapi_version": Value::Null,
        "hash": shapes::hex(block.hash.as_slice()),
        "logs_bloom": shapes::hex(block.logs_bloom.as_slice()),
        "name": shapes::record_file_name(block.consensus_timestamp),
        "number": block.number,
        "previous_hash": shapes::hex(block.parent_hash.as_slice()),
        "size": size(chain, block),
        "timestamp": timestamp_range(block.consensus_timestamp, Some(last)),
    })
}

/// Bytes of the transactions the block carries. A record file holds more than that; Hanvil has no
/// record files, and this is the number it can produce from what it stores.
fn size(chain: &Chain, block: &Block) -> usize {
    block
        .transactions
        .iter()
        .filter_map(|hash| chain.transaction(hash))
        .map(|tx| match &tx.body {
            TxBody::Signed(raw) => raw.len(),
            TxBody::Unsigned(unsigned) => unsigned.input.len(),
        })
        .sum()
}
