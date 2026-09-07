//! `/api/v1/accounts/{idOrAliasOrEvmAddress}` — the endpoint the harness resolves an ephemeral
//! signer through (`hedera-harness` PR #39, `waitForAccount`). A 404 means "not yet"; anything
//! else means the caller's URL is wrong.

use axum::extract::{Path, State};
use axum::response::Json;
use serde_json::{Value, json};

use super::shapes::{self, AUTO_RENEW_PERIOD, Reference, timestamp};
use super::{Answer, Error, Order, Params, page, transactions};
use crate::serve::Shared;
use crate::state::{Account, Chain, Timestamp};

/// `GET /api/v1/accounts/{idOrAliasOrEvmAddress}` — `openapi.yml:2148`
/// AccountBalanceTransactions: an AccountInfo with the account's transactions attached.
pub async fn get(State(chain): State<Shared>, Path(id): Path<String>, params: Params) -> Answer {
    let limit = params.limit()?;
    let order = params.order(Order::Desc)?;
    let with_transactions = params.flag("transactions", true)?;
    let reference = shapes::parse_reference(&id, "accountId")?;

    let chain = chain.read();
    let account = resolve(&chain, &reference).ok_or_else(Error::not_found)?;
    let mut body = info(&chain, account);
    let transactions: Vec<Value> = if with_transactions {
        let matched: Vec<Value> = chain
            .transactions()
            .filter(|tx| transactions::involves(&chain, tx, account.id))
            .map(|tx| transactions::record(&chain, tx))
            .collect();
        page(matched, order, limit)
    } else {
        Vec::new()
    };
    body["transactions"] = Value::Array(transactions);
    body["links"] = shapes::links();
    Ok(Json(body))
}

/// `GET /api/v1/accounts/{id}/tokens`. Hanvil emulates no token service, so the list is empty for
/// every account that exists, and a 404 for one that does not.
pub async fn tokens(State(chain): State<Shared>, Path(id): Path<String>) -> Answer {
    let reference = shapes::parse_reference(&id, "accountId")?;
    let chain = chain.read();
    resolve(&chain, &reference).ok_or_else(Error::not_found)?;
    Ok(Json(json!({ "tokens": [], "links": shapes::links() })))
}

fn resolve<'a>(chain: &'a Chain, reference: &Reference) -> Option<&'a Account> {
    match reference {
        Reference::Entity(id) => chain.account(*id),
        Reference::Evm(address) => chain.account_by_evm(address),
    }
}

/// `openapi.yml:1973` AccountInfo. Every field in the schema's required list is present.
pub(super) fn info(chain: &Chain, account: &Account) -> Value {
    json!({
        "account": account.id.to_string(),
        // The mirror's `alias` is the base32 key alias an auto-created account carries. Hanvil
        // mints EVM-address aliases only, which `evm_address` already reports.
        "alias": Value::Null,
        "auto_renew_period": AUTO_RENEW_PERIOD,
        "balance": {
            "timestamp": timestamp(chain.latest_block().consensus_timestamp).as_str(),
            "balance": account.balance.0,
            "tokens": [],
        },
        "created_timestamp": timestamp(account.created_at).as_str(),
        "decline_reward": false,
        "delegation_address": Value::Null,
        "deleted": account.deleted,
        "ethereum_nonce": account.nonce,
        "evm_address": shapes::hex(account.evm_address().as_slice()),
        "expiry_timestamp": timestamp(Timestamp {
            secs: account.created_at.secs + AUTO_RENEW_PERIOD,
            nanos: account.created_at.nanos,
        }).as_str(),
        "key": shapes::key(account.key.as_ref()),
        "max_automatic_token_associations": 0,
        "memo": account.memo,
        "pending_reward": 0,
        "receiver_sig_required": false,
        "staked_account_id": Value::Null,
        "staked_node_id": Value::Null,
        "stake_period_start": Value::Null,
    })
}
