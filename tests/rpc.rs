//! JSON-RPC end to end: deploy the Counter fixture with a locally signed legacy transaction,
//! call it, read logs, snapshot and revert, travel in time.

mod common;

use alloy_primitives::{Address, B256, U256, keccak256};
use serde_json::{Value, json};

use common::{
    Node, SENDER, WEIBAR_PER_TINYBAR, counter_fixture, hex_u64, hex_u256, selector, send,
    send_with_gas, sign_legacy_offering,
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

/// An address with no code answers a call with success and empty data, so a call to an unemulated
/// system contract would read as having worked. Genesis etches a reverting stub at each instead.
#[test]
fn system_contracts_revert_with_a_decodable_reason() {
    let node = Node::boot();
    for (address, call, reason) in [
        (
            "0x0000000000000000000000000000000000000167",
            "createFungibleToken()",
            "hanvil: HTS system contract not emulated; see README#system-contracts",
        ),
        (
            "0x0000000000000000000000000000000000000168",
            "tinycentsToTinybars(uint256)",
            "hanvil: exchange rate system contract not emulated; see README#system-contracts",
        ),
        (
            "0x0000000000000000000000000000000000000169",
            "getPseudorandomSeed()",
            "hanvil: PRNG system contract not emulated; see README#system-contracts",
        ),
    ] {
        let error = node.error(
            "eth_call",
            json!([{
                "to": address,
                "data": format!("0x{}", hex::encode(selector(call))),
            }, "latest"]),
        );
        assert_eq!(
            error["code"],
            json!(3),
            "geth revert shape, so viem decodes it"
        );
        assert_eq!(
            error["message"],
            json!(format!("execution reverted: {reason}"))
        );
        assert!(
            error["data"].as_str().unwrap().starts_with("0x08c379a0"),
            "Error(string) selector"
        );
    }
}

/// `--state` writes the chain on exit and reads it back at boot, so a second run continues the
/// first rather than starting from genesis.
#[test]
fn state_survives_a_restart() {
    let file = std::env::temp_dir().join(format!("hanvil-state-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&file);
    let path = file.to_str().unwrap();

    let contract = {
        let node = Node::boot_with(&["--state", path]);
        let init_code = hex::decode(
            counter_fixture()["bytecode"]
                .as_str()
                .unwrap()
                .trim_start_matches("0x"),
        )
        .unwrap();
        let receipt = send(&node, None, init_code, 0);
        let contract: Address = receipt["contractAddress"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        send(&node, Some(contract), selector("increment()").to_vec(), 0);
        assert_eq!(node.result("eth_blockNumber", json!([])), json!("0x2"));
        node.shutdown();
        contract
    };

    assert!(file.exists(), "--state wrote the chain on exit");

    let node = Node::boot_with(&["--state", path]);
    assert_eq!(
        node.result("eth_blockNumber", json!([])),
        json!("0x2"),
        "the reloaded chain keeps its height"
    );
    let count_call = json!({
        "to": format!("{contract:#x}"),
        "data": format!("0x{}", hex::encode(selector("count()"))),
    });
    assert_eq!(
        hex_u64(&node.result("eth_call", json!([count_call, "latest"]))),
        1,
        "contract storage came back with it"
    );
    assert_ne!(
        node.result("eth_getCode", json!([format!("{contract:#x}"), "latest"])),
        json!("0x"),
        "so did the deployed code"
    );
    drop(node);
    let _ = std::fs::remove_file(&file);
}

/// SIGTERM writes the state file too. `docker stop`, a process supervisor and a cancelled CI job
/// all send it, and a node that only listened for ctrl-c dropped the whole chain without a word.
#[test]
fn sigterm_writes_the_state_file() {
    let file = std::env::temp_dir().join(format!("hanvil-sigterm-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&file);
    let path = file.to_str().unwrap();

    {
        let node = Node::boot_with(&["--state", path]);
        send(&node, Some(Address::ZERO), Vec::new(), 1);
        assert_eq!(node.result("eth_blockNumber", json!([])), json!("0x1"));
        node.shutdown_with("TERM");
    }

    assert!(file.exists(), "SIGTERM wrote the chain on exit");
    let node = Node::boot_with(&["--state", path]);
    assert_eq!(
        node.result("eth_blockNumber", json!([])),
        json!("0x1"),
        "the chain SIGTERM left behind boots again"
    );
    drop(node);
    let _ = std::fs::remove_file(&file);
}

/// `--block-time` advances the chain on its own. Transactions are unaffected: they still mine
/// immediately rather than waiting for the interval.
#[test]
fn block_time_mines_empty_blocks_on_an_interval() {
    let node = Node::boot_with(&["--block-time", "1"]);
    let start = hex_u64(&node.result("eth_blockNumber", json!([])));

    // A transaction still mines at once, without waiting for the next tick.
    send(&node, Some(SENDER.parse().unwrap()), Vec::new(), 1);
    let after_tx = hex_u64(&node.result("eth_blockNumber", json!([])));
    assert!(after_tx > start, "automine is unchanged by --block-time");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if hex_u64(&node.result("eth_blockNumber", json!([]))) > after_tx {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    panic!("no empty block was mined within 10s at --block-time 1");
}

/// Hedera's relay refuses a transaction whose gas limit is over `MAX_TRANSACTION_GAS_LIMIT`
/// (15,000,000 by default) and caps an `eth_call` that asks for more instead of refusing it
/// (relay `docs/configuration.md:82`). Hanvil mined a 20,000,000-gas transaction, so a contract
/// that deployed here would have been rejected on Hedera.
#[test]
fn a_gas_limit_over_the_network_maximum_is_refused_and_a_call_is_capped() {
    let node = Node::boot();
    let payee: Address = "0x00000000000000000000000000000000000003ea"
        .parse()
        .unwrap();

    let raw = common::sign_legacy_with_gas(&node, Some(payee), Vec::new(), 1, 15_000_001);
    let error = node.error("eth_sendRawTransaction", json!([raw]));
    assert_eq!(error["code"], json!(-32005));
    assert_eq!(
        error["message"],
        json!("Transaction gas limit '0xe4e1c1' exceeds block gas limit '15000000'")
    );
    assert_eq!(node.result("eth_blockNumber", json!([])), json!("0x0"));

    // The maximum itself is accepted.
    let receipt = send_with_gas(&node, Some(payee), Vec::new(), 1, 15_000_000);
    assert_eq!(receipt["status"], json!("0x1"));
    assert_eq!(receipt["gasUsed"], json!("0x5208"));

    // A call asking for more is capped to the maximum, not refused.
    let called = node.result(
        "eth_call",
        json!([{ "from": SENDER, "to": format!("{payee:#x}"), "gas": "0x1c9c380" }, "latest"]),
    );
    assert_eq!(called, json!("0x"));

    // Blocks report the same number the rejection names.
    let block = node.result("eth_getBlockByNumber", json!(["latest", false]));
    assert_eq!(block["gasLimit"], json!("0xe4e1c0"));
}

/// `ethers`' `contract.on(...)` and `viem`'s `createEventFilter` poll this family over plain HTTP
/// when there is no WebSocket, and the relay serves it (`openrpc.json:885-1005`). A filter reports
/// only what arrived since the last poll, so the second poll of a quiet chain is empty.
#[test]
fn a_log_filter_reports_each_event_once() {
    let node = Node::boot();
    let fixture = counter_fixture();
    let init_code = hex::decode(
        fixture["bytecode"]
            .as_str()
            .unwrap()
            .trim_start_matches("0x"),
    )
    .expect("fixture bytecode is hex");
    let receipt = send(&node, None, init_code, 0);
    let contract: Address = receipt["contractAddress"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    // No toBlock, so the filter follows the head rather than pinning to it.
    let id = node.result(
        "eth_newFilter",
        json!([{ "address": format!("{contract:#x}") }]),
    );
    assert_eq!(
        node.result("eth_getFilterChanges", json!([id])),
        json!([]),
        "nothing has happened yet"
    );

    send(&node, Some(contract), selector("increment()").to_vec(), 0);
    let changes = node.result("eth_getFilterChanges", json!([id]));
    assert_eq!(changes.as_array().unwrap().len(), 1, "one Incremented log");
    assert_eq!(
        changes[0]["topics"][0],
        json!(format!("{:#x}", keccak256("Incremented(address,uint256)")))
    );
    assert_eq!(
        node.result("eth_getFilterChanges", json!([id])),
        json!([]),
        "a poll drains what it read; the same log must not arrive twice"
    );

    // eth_getFilterLogs ignores the cursor and re-reads everything the filter matches.
    assert_eq!(
        node.result("eth_getFilterLogs", json!([id]))
            .as_array()
            .unwrap()
            .len(),
        1
    );

    assert_eq!(node.result("eth_uninstallFilter", json!([id])), json!(true));
    assert_eq!(
        node.result("eth_uninstallFilter", json!([id])),
        json!(false),
        "uninstalling twice is false, not an error"
    );
    assert_eq!(
        node.error("eth_getFilterChanges", json!([id]))["code"],
        json!(-32000),
        "polling a filter that is gone is an error, not an empty array"
    );
    node.shutdown();
}

/// `anvil_mine(blocks, interval)` spaces the blocks. Accepting the interval and mining
/// back-to-back looked like it worked and produced a chain where no time had passed.
#[test]
fn anvil_mine_spaces_blocks_by_its_interval() {
    let node = Node::boot();
    node.result("anvil_mine", json!(["0x3", "0x1e"]));
    assert_eq!(node.result("eth_blockNumber", json!([])), json!("0x3"));
    let first = hex_u64(&node.result("eth_getBlockByNumber", json!(["0x1", false]))["timestamp"]);
    let last = hex_u64(&node.result("eth_getBlockByNumber", json!(["0x3", false]))["timestamp"]);
    assert_eq!(last - first, 60, "two 30-second gaps between three blocks");
    node.shutdown();
}

/// The relay answers these two the way this asserts, and Hanvil answered both differently: a
/// success where the relay errors, and an error where the relay answers.
#[test]
fn net_peer_count_and_submit_work_match_the_relay() {
    let node = Node::boot();
    assert_eq!(
        node.error("net_peerCount", json!([]))["code"],
        json!(-32601),
        "the relay's summary for net_peerCount is `Always returns UNSUPPORTED_METHOD error`"
    );
    assert_eq!(node.result("eth_submitWork", json!([])), json!(false));
    node.shutdown();
}

/// A send the node will not take leaves no block and no receipt, so the error object is all the
/// caller gets. `hanvil_rejections` is where it can be read back.
#[test]
fn hanvil_rejections_reports_what_no_receipt_records() {
    let node = Node::boot();
    let before = node.result("eth_blockNumber", json!([]));

    let refused = node.error(
        "eth_sendTransaction",
        json!([{ "from": SENDER, "to": "0x0000000000000000000000000000000000000001",
                 "value": "0x1" }]),
    );
    assert_eq!(refused["code"], json!(-32602));
    assert_eq!(
        node.result("eth_blockNumber", json!([])),
        before,
        "a refusal mines nothing"
    );

    let rows = node.result("hanvil_rejections", json!([]));
    let rows = rows.as_array().expect("array");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["kind"], "ETHEREUMTRANSACTION");
    assert_eq!(
        rows[0]["code"],
        Value::Null,
        "the relay has no response code"
    );
    assert_eq!(
        rows[0]["from"].as_str().expect("sender").to_lowercase(),
        SENDER.to_lowercase()
    );
    assert!(
        rows[0]["reason"]
            .as_str()
            .expect("reason")
            .contains("not a multiple of 10^10"),
        "{}",
        rows[0]["reason"]
    );
    node.shutdown();
}

/// `gasPrice` on a mined transaction is the price paid, not the ceiling the sender offered. It
/// rendered `maxFeePerGas`, so a caller computing a fee as `gasPrice * gasUsed` overstated it by
/// whatever margin the sender left — 20 % on a plain viem transaction. The receipt was already
/// right; only the transaction object was wrong.
#[test]
fn gas_price_on_a_mined_transaction_is_the_price_paid() {
    let node = Node::boot();
    let network = hex_u256(&node.result("eth_gasPrice", json!([])));
    let offered = network * U256::from(2);

    let raw = sign_legacy_offering(&node, Address::ZERO, offered);
    let hash = node.result("eth_sendRawTransaction", json!([raw]));
    let receipt = node.result("eth_getTransactionReceipt", json!([hash]));
    let tx = node.result("eth_getTransactionByHash", json!([hash]));

    assert_eq!(
        hex_u256(&receipt["effectiveGasPrice"]),
        network,
        "the chain charges the network price, not what was offered"
    );
    assert_eq!(
        tx["gasPrice"], receipt["effectiveGasPrice"],
        "the transaction reports the price it paid"
    );
    assert_ne!(
        hex_u256(&tx["gasPrice"]),
        offered,
        "and not the price it offered"
    );

    // The same object inside a full block.
    let block = node.result(
        "eth_getBlockByNumber",
        json!([receipt["blockNumber"].clone(), true]),
    );
    assert_eq!(
        block["transactions"][0]["gasPrice"], receipt["effectiveGasPrice"],
        "including the copy embedded in the block"
    );
}

/// `eth_accounts` lists the accounts `eth_sendTransaction` will send for. It answered `[]` while
/// accepting all thirty, so hardhat's `getSigners()` and ethers' `provider.getSigner()` found
/// nothing on a node that would have served them.
#[test]
fn eth_accounts_names_the_accounts_it_will_send_for() {
    let node = Node::boot();
    let accounts = node.result("eth_accounts", json!([]));
    let accounts = accounts.as_array().expect("an array");
    assert_eq!(accounts.len(), 30, "ten of each key type");
    assert_eq!(
        accounts[0], "0x00000000000000000000000000000000000003ea",
        "0.0.1002 first: id order, which is hiero-local-node's numbering"
    );
    assert_eq!(
        accounts[10], SENDER,
        "the first alias account is 0.0.1012, the one hardhat and viem use"
    );

    // Every listed account is one the node will actually send for, with no impersonation.
    for account in accounts {
        node.result(
            "eth_sendTransaction",
            json!([{ "from": account, "to": SENDER, "value": "0x0", "gas": "0x5208" }]),
        );
    }
    // And an address that is not listed still is not.
    let refused = node.error(
        "eth_sendTransaction",
        json!([{ "from": "0x1111111111111111111111111111111111111111", "to": SENDER, "value": "0x0" }]),
    );
    assert!(
        refused["message"]
            .as_str()
            .unwrap()
            .contains("anvil_impersonateAccount")
    );

    let two = Node::boot_with(&["--accounts", "2"]);
    assert_eq!(
        two.result("eth_accounts", json!([]))
            .as_array()
            .unwrap()
            .len(),
        6,
        "--accounts scales the list"
    );
}
