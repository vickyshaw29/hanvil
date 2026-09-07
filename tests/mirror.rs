//! Mirror REST end to end: deploy and call a contract over JSON-RPC, then read the same events
//! back through the mirror endpoints the harness uses. The golden files pin the field sets; a
//! field that appears, disappears or is renamed fails the test.

mod common;

use std::collections::BTreeMap;

use alloy_primitives::Address;
use serde_json::{Value, json};

use common::{Node, SENDER, counter_fixture, selector, send};

/// Values that change with the clock, the port or the run. The shape is the assertion; the
/// content is checked by the assertions above each golden.
const VOLATILE: [&str; 14] = [
    "balance",
    "block_hash",
    "bloom",
    "consensus_timestamp",
    "created_timestamp",
    "expiration_time",
    "expiry_timestamp",
    "logs_bloom",
    "name",
    "previous_hash",
    "hash",
    "timestamp",
    "transaction_id",
    "valid_start_timestamp",
];

/// Replace every volatile value with its key name, and sort keys, so the golden captures the
/// field set and the stable values and nothing else. `revm-inspector` turns on `serde_json`'s
/// `preserve_order` feature, so insertion order would otherwise leak into the diff.
fn normalise(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, child)| {
                    let child = if VOLATILE.contains(&key.as_str()) {
                        json!(format!("<{key}>"))
                    } else {
                        normalise(child)
                    };
                    (key.clone(), child)
                })
                .collect::<BTreeMap<String, Value>>()
                .into_iter()
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(normalise).collect()),
        other => other.clone(),
    }
}

/// Compare against `tests/golden/{name}.json`, writing it first when `UPDATE_GOLDEN=1`.
fn golden(name: &str, value: &Value) {
    let path = format!("{}/tests/golden/{name}.json", env!("CARGO_MANIFEST_DIR"));
    let actual = format!("{:#}\n", normalise(value));
    if std::env::var("UPDATE_GOLDEN").is_ok() {
        std::fs::write(&path, &actual).expect("write golden");
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("{path} is missing; rerun with UPDATE_GOLDEN=1"));
    assert_eq!(actual, expected, "{name} no longer matches its golden");
}

#[test]
fn accounts_resolve_by_id_and_by_evm_address() {
    let node = Node::boot();
    let (status, by_id) = node.get("/api/v1/accounts/0.0.1012");
    assert_eq!(status, 200);
    let (_, by_evm) = node.get(&format!("/api/v1/accounts/{SENDER}"));
    assert_eq!(by_id, by_evm, "an alias and its id name one account");
    assert_eq!(by_id["account"], json!("0.0.1012"));
    assert_eq!(by_id["evm_address"], json!(SENDER));
    assert_eq!(by_id["balance"]["balance"], json!(1_000_000_000_000u64));
    assert_eq!(by_id["key"]["_type"], json!("ECDSA_SECP256K1"));
    assert_eq!(by_id["deleted"], json!(false));
    golden("account", &by_id);

    // A long-zero account resolves too, and its key is the ED25519 one for 0.0.1022.
    let (status, ed) = node.get("/api/v1/accounts/0.0.1022");
    assert_eq!(status, 200);
    assert_eq!(ed["key"]["_type"], json!("ED25519"));
}

#[test]
fn an_account_that_does_not_exist_is_a_404_in_the_spec_shape() {
    let node = Node::boot();
    let (status, body) = node.get("/api/v1/accounts/0.0.999999");
    assert_eq!(status, 404, "PR #39's reader waits on 404 and stops on 4xx");
    assert_eq!(
        body,
        json!({ "_status": { "messages": [{ "message": "Not found" }] } })
    );

    // A path Hanvil does not serve says so, rather than looking like an empty network.
    let (status, body) = node.get("/api/v1/tokens/0.0.1");
    assert_eq!(status, 404);
    assert!(
        body["_status"]["messages"][0]["detail"]
            .as_str()
            .expect("detail names the path")
            .contains("/api/v1/tokens/0.0.1")
    );
}

#[test]
fn transactions_carry_the_hedera_view_of_an_evm_transfer() {
    let node = Node::boot();
    let recipient: Address = "0x00000000000000000000000000000000000003ea"
        .parse()
        .expect("0.0.1002 long-zero");
    let receipt = send(&node, Some(recipient), Vec::new(), 100_000_000);
    assert_eq!(receipt["status"], json!("0x1"));

    let (status, list) = node.get("/api/v1/transactions");
    assert_eq!(status, 200);
    let tx = &list["transactions"][0];
    assert_eq!(tx["name"], json!("ETHEREUMTRANSACTION"));
    assert_eq!(tx["result"], json!("SUCCESS"));
    assert_eq!(tx["node"], json!("0.0.3"));
    assert_eq!(tx["charged_tx_fee"], json!(21_000 * 71));

    // The fee leaves the payer and lands on 0.0.98; the value lands on 0.0.1002. Nothing is lost.
    let moved: BTreeMap<&str, i64> = tx["transfers"]
        .as_array()
        .expect("transfers")
        .iter()
        .map(|entry| {
            (
                entry["account"].as_str().expect("account id"),
                entry["amount"].as_i64().expect("tinybar"),
            )
        })
        .collect();
    assert_eq!(moved["0.0.98"], 21_000 * 71);
    assert_eq!(moved["0.0.1002"], 100_000_000);
    assert_eq!(moved["0.0.1012"], -(100_000_000 + 21_000 * 71));
    assert_eq!(moved.values().sum::<i64>(), 0, "transfers balance to zero");
    golden("transaction", tx);

    // The same record by its transaction id, which is what the harness holds after a receipt.
    let id = tx["transaction_id"].as_str().expect("transaction id");
    let (status, one) = node.get(&format!("/api/v1/transactions/{id}"));
    assert_eq!(status, 200);
    assert_eq!(one["transactions"][0], *tx);

    // The SDK's own form is a 400 here, not an empty answer: PR #39 stops polling on it.
    let sdk_form = id.replacen('-', "@", 1);
    let sdk_form = format!(
        "{}.{}",
        &sdk_form[..sdk_form.rfind('-').expect("nanos")],
        &sdk_form[sdk_form.rfind('-').expect("nanos") + 1..]
    );
    let (status, body) = node.get(&format!("/api/v1/transactions/{sdk_form}"));
    assert_eq!(status, 400, "the mirror rejects 0.0.x@sss.nnn");
    assert!(
        body["_status"]["messages"][0]["message"]
            .as_str()
            .expect("message")
            .contains("shard.realm.num-sss-nnn")
    );

    // An account's own page lists it, and `transactions=false` leaves it out.
    let (_, account) = node.get("/api/v1/accounts/0.0.1012");
    assert_eq!(account["transactions"][0]["transaction_id"], json!(id));
    let (_, without) = node.get("/api/v1/accounts/0.0.1012?transactions=false");
    assert_eq!(without["transactions"], json!([]));
    let (_, filtered) = node.get("/api/v1/transactions?account.id=0.0.1002");
    assert_eq!(filtered["transactions"].as_array().expect("list").len(), 1);
    let (_, other) = node.get("/api/v1/transactions?account.id=0.0.1003");
    assert_eq!(other["transactions"], json!([]));

    // A real Hedera type Hanvil never records has no matches; a typo is a caller error.
    let (status, none) = node.get("/api/v1/transactions?transactiontype=CRYPTOTRANSFER");
    assert_eq!(status, 200);
    assert_eq!(none["transactions"], json!([]));
    let (status, mine) = node.get("/api/v1/transactions?transactiontype=ethereumtransaction");
    assert_eq!(status, 200);
    assert_eq!(mine["transactions"].as_array().expect("list").len(), 1);
    let (status, _) = node.get("/api/v1/transactions?transactiontype=CRYPTOTRANSFERS");
    assert_eq!(status, 400, "not a transaction type the mirror knows");
}

#[test]
fn an_account_created_by_a_transfer_resolves_by_its_evm_address() {
    // What PR #39's `waitForAccount` does after provisioning a signer: read the account back
    // from the mirror by the EVM address the app knows it as.
    let node = Node::boot();
    let fresh: Address = "0x00000000000000000000000000000000000000ff"
        .parse()
        .expect("address");
    let (status, _) = node.get(&format!("/api/v1/accounts/{fresh:#x}"));
    assert_eq!(status, 404, "nothing there before the transfer");

    send(&node, Some(fresh), Vec::new(), 5_000_000);

    let (status, account) = node.get(&format!("/api/v1/accounts/{fresh:#x}"));
    assert_eq!(status, 200, "the transfer created the account");
    assert_eq!(account["evm_address"], json!(format!("{fresh:#x}")));
    assert_eq!(account["balance"]["balance"], json!(5_000_000));
    assert_eq!(account["key"], json!(null), "a hollow account holds no key");
    // It got the next free id after the predefined thirty.
    assert_eq!(account["account"], json!("0.0.1032"));
}

#[test]
fn contract_results_and_logs_come_back_by_hash_and_by_id() {
    let node = Node::boot();
    let fixture = counter_fixture();
    let bytecode = hex::decode(
        fixture["bytecode"]
            .as_str()
            .expect("fixture bytecode")
            .trim_start_matches("0x"),
    )
    .expect("hex");
    let deploy = send(&node, None, bytecode, 0);
    let address: Address = deploy["contractAddress"]
        .as_str()
        .expect("deployed")
        .parse()
        .expect("address");

    // increment() emits Counted(uint256), so the log endpoints have something to answer with.
    let call = send(&node, Some(address), selector("increment()").to_vec(), 0);
    let hash = call["transactionHash"].as_str().expect("hash");

    let (status, result) = node.get(&format!("/api/v1/contracts/results/{hash}"));
    assert_eq!(status, 200);
    assert_eq!(result["status"], json!("0x1"));
    assert_eq!(result["result"], json!("SUCCESS"));
    assert_eq!(
        result["gas_used"],
        call["gasUsed"].as_str().map_or(json!(null), |gas| json!(
            u64::from_str_radix(gas.trim_start_matches("0x"), 16).expect("gas")
        ),)
    );
    assert_eq!(result["to"], json!(format!("{address:#x}")));
    assert_eq!(result["logs"].as_array().expect("logs").len(), 1);
    golden("contract_result", &result);

    let contract_id = result["contract_id"].as_str().expect("contract id");
    let (status, contract) = node.get(&format!("/api/v1/contracts/{contract_id}"));
    assert_eq!(status, 200);
    assert_eq!(contract["evm_address"], json!(format!("{address:#x}")));
    let code = node.result("eth_getCode", json!([format!("{address:#x}"), "latest"]));
    assert_eq!(
        contract["runtime_bytecode"], code,
        "same code as eth_getCode"
    );
    golden("contract", &contract);

    // The contract is reachable by its EVM address too, and lists both of its results.
    let (status, by_address) = node.get(&format!("/api/v1/contracts/{address:#x}/results"));
    assert_eq!(status, 200);
    assert_eq!(
        by_address["results"].as_array().expect("results").len(),
        2,
        "the create and the call"
    );

    let (status, logs) = node.get("/api/v1/contracts/results/logs");
    assert_eq!(status, 200);
    let log = &logs["logs"][0];
    assert_eq!(log["address"], json!(format!("{address:#x}")));
    assert_eq!(log["contract_id"], json!(contract_id));
    assert_eq!(log["index"], json!(0));
    golden("contract_log", log);
}

#[test]
fn blocks_and_network_answer_what_a_client_bootstraps_with() {
    let node = Node::boot();
    node.result("evm_mine", json!([]));

    let (status, block) = node.get("/api/v1/blocks/1");
    assert_eq!(status, 200);
    assert_eq!(block["number"], json!(1));
    assert_eq!(block["count"], json!(0));
    assert!(
        block["name"]
            .as_str()
            .expect("record file name")
            .ends_with("Z.rcd")
    );
    golden("block", &block);

    let (_, by_hash) = node.get(&format!(
        "/api/v1/blocks/{}",
        block["hash"].as_str().expect("hash")
    ));
    assert_eq!(by_hash, block);

    let (_, list) = node.get("/api/v1/blocks?limit=1&order=desc");
    assert_eq!(list["blocks"][0]["number"], json!(1));
    let (_, ascending) = node.get("/api/v1/blocks?limit=1&order=asc");
    assert_eq!(ascending["blocks"][0]["number"], json!(0));
    let (status, _) = node.get("/api/v1/blocks?limit=0");
    assert_eq!(status, 400, "limit is 1..=100");

    let (status, nodes) = node.get("/api/v1/network/nodes");
    assert_eq!(status, 200);
    assert_eq!(nodes["nodes"][0]["node_account_id"], json!("0.0.3"));
    golden("network_nodes", &nodes);

    let (status, fees) = node.get("/api/v1/network/fees");
    assert_eq!(status, 200);
    assert_eq!(fees["fees"][0]["gas"], json!(71));
    let (status, rate) = node.get("/api/v1/network/exchangerate");
    assert_eq!(status, 200);
    assert_eq!(rate["current_rate"]["hbar_equivalent"], json!(30_000));
}
