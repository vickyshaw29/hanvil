//! JSON-RPC end to end: deploy the Counter fixture with a locally signed legacy transaction,
//! call it, read logs, snapshot and revert, travel in time.

mod common;

use alloy_primitives::{Address, B256, U256, keccak256};
use serde_json::{Value, json};

use common::{
    Node, SENDER, WEIBAR_PER_TINYBAR, counter_fixture, hex_u64, hex_u256, selector, send,
    send_with_gas,
};

#[test]
fn deploy_call_log_snapshot_revert_and_time_travel() {
    let node = Node::boot();
    let fixture = counter_fixture();
    let init_code = hex::decode(
        fixture["bytecode"]
            .as_str()
            .unwrap()
            .trim_start_matches("0x"),
    )
    .unwrap();

    assert_eq!(node.result("eth_chainId", json!([])), json!("0x12a"));
    assert_eq!(node.result("eth_blockNumber", json!([])), json!("0x0"));
    let balance_before = hex_u256(&node.result("eth_getBalance", json!([SENDER, "latest"])));
    assert_eq!(
        balance_before,
        U256::from(10_000u64) * U256::from(10u64).pow(U256::from(18))
    );

    // Deploy.
    let receipt = send(&node, None, init_code, 0);
    assert_eq!(receipt["status"], json!("0x1"));
    assert_eq!(receipt["blockNumber"], json!("0x1"));
    let contract: Address = receipt["contractAddress"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let code = node.result("eth_getCode", json!([format!("{contract:#x}"), "latest"]));
    assert_eq!(code, json!(fixture["deployedBytecode"]));

    // count() == 0, then increment() emits one log.
    let count_call = json!({ "to": format!("{contract:#x}"), "data": format!("0x{}", hex::encode(selector("count()"))) });
    assert_eq!(
        hex_u64(&node.result("eth_call", json!([count_call, "latest"]))),
        0
    );
    let receipt = send(&node, Some(contract), selector("increment()").to_vec(), 0);
    assert_eq!(receipt["status"], json!("0x1"));
    assert_eq!(receipt["logs"].as_array().unwrap().len(), 1);
    let topic0 = format!("{:#x}", keccak256("Incremented(address,uint256)"));
    assert_eq!(receipt["logs"][0]["topics"][0], json!(topic0));
    assert_eq!(
        hex_u64(&node.result("eth_call", json!([count_call, "latest"]))),
        1
    );

    // eth_getLogs by address and by topic; the block carries the bloom.
    let logs = node.result(
        "eth_getLogs",
        json!([{ "fromBlock": "0x0", "toBlock": "latest", "address": format!("{contract:#x}"), "topics": [topic0] }]),
    );
    assert_eq!(logs.as_array().unwrap().len(), 1);
    assert_eq!(logs[0]["blockNumber"], json!("0x2"));
    let block = node.result("eth_getBlockByNumber", json!(["0x2", true]));
    assert_eq!(block["transactions"][0]["from"], json!(SENDER));
    assert_ne!(block["logsBloom"], json!(format!("0x{}", "0".repeat(512))));

    // Revert reason and custom error come back as code 3 with data.
    let fail_call = json!({ "to": format!("{contract:#x}"), "data": format!("0x{}", hex::encode(selector("fail()"))) });
    let error = node.error("eth_call", json!([fail_call, "latest"]));
    assert_eq!(error["code"], json!(3));
    assert_eq!(error["message"], json!("execution reverted: Counter: fail"));
    assert!(error["data"].as_str().unwrap().starts_with("0x08c379a0"));
    let mut too_high = selector("incrementBy(uint256)").to_vec();
    too_high.extend_from_slice(&U256::from(500).to_be_bytes::<32>());
    let error = node.error(
        "eth_call",
        json!([{ "to": format!("{contract:#x}"), "data": format!("0x{}", hex::encode(&too_high)) }, "latest"]),
    );
    assert_eq!(error["code"], json!(3));
    let expected_selector = format!("0x{}", hex::encode(selector("TooHigh(uint256,uint256)")));
    assert!(
        error["data"]
            .as_str()
            .unwrap()
            .starts_with(&expected_selector)
    );

    // Snapshot, mutate, revert: count, block number and nonce all go back.
    let snapshot = node.result("evm_snapshot", json!([]));
    assert_eq!(snapshot, json!("0x0"));
    send(&node, Some(contract), selector("increment()").to_vec(), 0);
    assert_eq!(
        hex_u64(&node.result("eth_call", json!([count_call, "latest"]))),
        2
    );
    assert_eq!(node.result("eth_blockNumber", json!([])), json!("0x3"));
    assert_eq!(node.result("evm_revert", json!([snapshot])), json!(true));
    assert_eq!(
        hex_u64(&node.result("eth_call", json!([count_call, "latest"]))),
        1
    );
    assert_eq!(node.result("eth_blockNumber", json!([])), json!("0x2"));
    assert_eq!(
        node.result("eth_getTransactionCount", json!([SENDER, "latest"])),
        json!("0x2")
    );
    assert_eq!(
        node.result("evm_revert", json!([snapshot])),
        json!(false),
        "consumed"
    );

    // Time travel: +3600 s then an empty block.
    let before =
        hex_u64(&node.result("eth_getBlockByNumber", json!(["latest", false]))["timestamp"]);
    assert_eq!(node.result("evm_increaseTime", json!([3600])), json!(3600));
    assert_eq!(node.result("evm_mine", json!([])), json!("0x0"));
    let after =
        hex_u64(&node.result("eth_getBlockByNumber", json!(["latest", false]))["timestamp"]);
    assert!(after >= before + 3600, "{after} < {before} + 3600");

    // Fees: sender paid gas * price; the fee collector 0.0.98 holds it.
    let paid = balance_before - hex_u256(&node.result("eth_getBalance", json!([SENDER, "latest"])));
    let collector = hex_u256(&node.result(
        "eth_getBalance",
        json!(["0x0000000000000000000000000000000000000062", "latest"]),
    ));
    assert_eq!(paid, collector, "fees are collected, not burned");
    assert_eq!(
        paid % U256::from(WEIBAR_PER_TINYBAR),
        U256::ZERO,
        "whole tinybar"
    );
}

#[test]
fn value_transfer_and_hollow_account() {
    let node = Node::boot();
    let fresh: Address = "0x1234567890123456789012345678901234567890"
        .parse()
        .unwrap();
    let receipt = send(&node, Some(fresh), Vec::new(), 100_000_000);
    assert_eq!(receipt["status"], json!("0x1"));
    assert_eq!(receipt["gasUsed"], json!("0x5208"));
    let balance =
        hex_u256(&node.result("eth_getBalance", json!([format!("{fresh:#x}"), "latest"])));
    assert_eq!(
        balance,
        U256::from(100_000_000u64) * U256::from(WEIBAR_PER_TINYBAR)
    );
}

#[test]
fn fractional_tinybar_value_is_rejected_before_execution() {
    let node = Node::boot();
    let error = node.error(
        "eth_call",
        json!([{ "from": SENDER, "to": "0x00000000000000000000000000000000000003ea", "value": "0x1" }, "latest"]),
    );
    assert_eq!(error["code"], json!(-32602));
    assert!(error["message"].as_str().unwrap().contains("10^10"));
}

#[test]
fn impersonated_and_unknown_senders() {
    let node = Node::boot();
    let ghost = "0x1111111111111111111111111111111111111111";
    let refused = node.error(
        "eth_sendTransaction",
        json!([{ "from": ghost, "to": SENDER, "value": "0x0" }]),
    );
    assert_eq!(refused["code"], json!(-32000));
    assert!(
        refused["message"]
            .as_str()
            .unwrap()
            .contains("anvil_impersonateAccount")
    );

    node.result("anvil_impersonateAccount", json!([ghost]));
    node.result("anvil_setBalance", json!([ghost, "0x21e19e0c9bab2400000"]));
    let hash = node.result(
        "eth_sendTransaction",
        json!([{ "from": ghost, "to": SENDER, "value": "0x2540be400", "gas": "0x5208" }]),
    );
    let receipt = node.result("eth_getTransactionReceipt", json!([hash]));
    assert_eq!(receipt["status"], json!("0x1"));
    assert_eq!(receipt["from"], json!(ghost));
    let tx = node.result("eth_getTransactionByHash", json!([hash]));
    assert_eq!(tx["value"], json!("0x2540be400"));
    assert_eq!(tx["type"], json!("0x0"));
}

#[test]
fn relay_unsupported_methods_and_historical_state() {
    let node = Node::boot();
    let error = node.error("eth_sign", json!([]));
    assert_eq!(error["code"], json!(-32601));
    assert_eq!(error["message"], json!("Method eth_sign not supported"));
    // Block 0 is the head at genesis, so it is answerable; after one block it is history.
    node.result("eth_getBalance", json!([SENDER, "0x0"]));
    node.result("evm_mine", json!([]));
    let error = node.error("eth_getBalance", json!([SENDER, "0x0"]));
    assert_eq!(error["code"], json!(-32000));
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("historical state")
    );
    assert_eq!(
        node.result("eth_getBlockByNumber", json!(["0x999", false])),
        Value::Null
    );
    let past_hash = B256::ZERO;
    assert_eq!(
        node.result(
            "eth_getBlockByHash",
            json!([format!("{past_hash:#x}"), false])
        ),
        Value::Null
    );
}

/// `ReceiptInfo` in the relay's openrpc.json lists these fifteen properties and no others. A
/// failed transaction gets the same set: neither the relay nor Anvil puts revert data on a
/// receipt, so a caller reads it from the `eth_call` error or the mirror's contract result.
#[test]
fn receipts_carry_the_relay_field_set_on_success_and_on_revert() {
    const RECEIPT_INFO: [&str; 15] = [
        "blockHash",
        "blockNumber",
        "contractAddress",
        "cumulativeGasUsed",
        "effectiveGasPrice",
        "from",
        "gasUsed",
        "logs",
        "logsBloom",
        "root",
        "status",
        "to",
        "transactionHash",
        "transactionIndex",
        "type",
    ];

    let node = Node::boot();
    let init_code = hex::decode(
        counter_fixture()["bytecode"]
            .as_str()
            .unwrap()
            .trim_start_matches("0x"),
    )
    .unwrap();

    let deployed = send(&node, None, init_code, 0);
    assert_eq!(deployed["status"], json!("0x1"));
    let contract: Address = deployed["contractAddress"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    // `fail()` reverts, so its gas cannot be estimated; 100k covers the revert.
    let reverted = send_with_gas(
        &node,
        Some(contract),
        selector("fail()").to_vec(),
        0,
        100_000,
    );
    assert_eq!(reverted["status"], json!("0x0"), "the call reverted");

    for receipt in [&deployed, &reverted] {
        let mut keys: Vec<&str> = receipt
            .as_object()
            .expect("receipt is an object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys, RECEIPT_INFO,
            "receipt fields are the relay's, exactly"
        );
    }
}

/// An address with no code answers a call with success and empty data, so an unemulated HTS call
/// would read as having worked. Genesis etches a reverting stub at `0x…0167` instead.
#[test]
fn hts_system_contract_reverts_with_a_decodable_reason() {
    let node = Node::boot();
    let error = node.error(
        "eth_call",
        json!([{
            "to": "0x0000000000000000000000000000000000000167",
            "data": format!("0x{}", hex::encode(selector("createFungibleToken()"))),
        }, "latest"]),
    );
    assert_eq!(
        error["code"],
        json!(3),
        "geth revert shape, so viem decodes it"
    );
    assert_eq!(
        error["message"],
        json!("execution reverted: hanvil: HTS system contract not emulated; see README#hts")
    );
    assert!(
        error["data"].as_str().unwrap().starts_with("0x08c379a0"),
        "Error(string) selector"
    );
}
