//! JSON-RPC end to end: boot the binary on a random port, deploy the Counter fixture with a
//! locally signed legacy transaction, call it, read logs, snapshot and revert, travel in time.
//! No HTTP client crate: a JSON-RPC request is one HTTP/1.1 POST over a TcpStream.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use alloy_consensus::{SignableTransaction as _, TxEnvelope, TxLegacy};
use alloy_eips::eip2718::Encodable2718 as _;
use alloy_primitives::{Address, B256, Bytes, Signature, TxKind, U256, keccak256};
use serde_json::{Value, json};

/// 0.0.1012, the first alias account: hiero-local-node's README lists it first for hardhat.
const KEY: &str = "0x105d050185ccb907fba04dd92d8de9e32c18305e097ab41dadda21489a211524";
const SENDER: &str = "0x67d8d32e9bf1a9968a5ff53b87d777aa8ebbee69";
const WEIBAR_PER_TINYBAR: u128 = 10_000_000_000;

struct Node {
    child: Child,
    port: u16,
}

impl Node {
    fn boot() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_hanvil"))
            .args(["--port", "0", "--mirror-port", "0", "--grpc-port", "0"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("hanvil binary starts");
        let stdout = child.stdout.take().expect("piped stdout");
        let mut lines = BufReader::new(stdout).lines();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut port = None;
        while Instant::now() < deadline {
            let Some(Ok(line)) = lines.next() else { break };
            if let Some(rest) = line.strip_prefix("JSON-RPC   http://127.0.0.1:") {
                port = rest.split_whitespace().next().and_then(|p| p.parse().ok());
            }
            if line.starts_with("Started in") {
                break;
            }
        }
        // Keep draining so the child never blocks on a full pipe.
        std::thread::spawn(move || for _ in lines {});
        Self {
            child,
            port: port.expect("banner names the bound JSON-RPC port"),
        }
    }

    fn rpc(&self, method: &str, params: Value) -> Value {
        let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }).to_string();
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).expect("connect");
        write!(
            stream,
            "POST / HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .expect("write request");
        let mut response = String::new();
        stream.read_to_string(&mut response).expect("read response");
        let (_, payload) = response.split_once("\r\n\r\n").expect("http body");
        serde_json::from_str(payload).expect("json body")
    }

    fn result(&self, method: &str, params: Value) -> Value {
        let response = self.rpc(method, params);
        assert!(
            response.get("error").is_none(),
            "{method} failed: {}",
            response["error"]
        );
        response["result"].clone()
    }

    fn error(&self, method: &str, params: Value) -> Value {
        let response = self.rpc(method, params);
        assert!(
            response.get("error").is_some(),
            "{method} unexpectedly succeeded: {}",
            response["result"]
        );
        response["error"].clone()
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn hex_u64(value: &Value) -> u64 {
    let text = value.as_str().expect("hex quantity");
    u64::from_str_radix(text.trim_start_matches("0x"), 16).expect("parses")
}

fn hex_u256(value: &Value) -> U256 {
    let text = value.as_str().expect("hex quantity");
    U256::from_str_radix(text.trim_start_matches("0x"), 16).expect("parses")
}

fn counter_fixture() -> Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/Counter.json");
    serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("json")
}

fn selector(signature: &str) -> [u8; 4] {
    let hash = keccak256(signature.as_bytes());
    [hash[0], hash[1], hash[2], hash[3]]
}

/// Sign a legacy transaction with the dev key; returns the EIP-2718 bytes as hex.
fn sign_legacy(node: &Node, to: Option<Address>, input: Vec<u8>, value_tinybar: u64) -> String {
    let nonce = hex_u64(&node.result("eth_getTransactionCount", json!([SENDER, "latest"])));
    let gas_price = hex_u256(&node.result("eth_gasPrice", json!([])));
    let mut call = json!({ "from": SENDER, "data": format!("0x{}", hex::encode(&input)) });
    if let Some(to) = to {
        call["to"] = json!(format!("{to:#x}"));
    }
    if value_tinybar > 0 {
        call["value"] = json!(format!("0x{:x}", u128::from(value_tinybar) * WEIBAR_PER_TINYBAR));
    }
    let gas = hex_u64(&node.result("eth_estimateGas", json!([call])));
    let tx = TxLegacy {
        chain_id: Some(298),
        nonce,
        gas_price: gas_price.to::<u128>(),
        gas_limit: gas,
        to: to.map_or(TxKind::Create, TxKind::Call),
        value: U256::from(u128::from(value_tinybar) * WEIBAR_PER_TINYBAR),
        input: Bytes::from(input),
    };
    let key = k256::ecdsa::SigningKey::from_slice(&hex::decode(&KEY[2..]).unwrap()).unwrap();
    let digest = tx.signature_hash();
    let (sig, recovery) = key.sign_prehash_recoverable(digest.as_slice()).unwrap();
    let r = U256::from_be_slice(&sig.r().to_bytes());
    let s = U256::from_be_slice(&sig.s().to_bytes());
    let signed = tx.into_signed(Signature::new(r, s, recovery.is_y_odd()));
    format!("0x{}", hex::encode(TxEnvelope::Legacy(signed).encoded_2718()))
}

fn send(node: &Node, to: Option<Address>, input: Vec<u8>, value_tinybar: u64) -> Value {
    let raw = sign_legacy(node, to, input, value_tinybar);
    let hash = node.result("eth_sendRawTransaction", json!([raw]));
    let receipt = node.result("eth_getTransactionReceipt", json!([hash]));
    assert!(!receipt.is_null(), "receipt is available immediately");
    receipt
}

#[test]
fn deploy_call_log_snapshot_revert_and_time_travel() {
    let node = Node::boot();
    let fixture = counter_fixture();
    let init_code = hex::decode(fixture["bytecode"].as_str().unwrap().trim_start_matches("0x")).unwrap();

    assert_eq!(node.result("eth_chainId", json!([])), json!("0x12a"));
    assert_eq!(node.result("eth_blockNumber", json!([])), json!("0x0"));
    let balance_before = hex_u256(&node.result("eth_getBalance", json!([SENDER, "latest"])));
    assert_eq!(balance_before, U256::from(10_000u64) * U256::from(10u64).pow(U256::from(18)));

    // Deploy.
    let receipt = send(&node, None, init_code, 0);
    assert_eq!(receipt["status"], json!("0x1"));
    assert_eq!(receipt["blockNumber"], json!("0x1"));
    let contract: Address = receipt["contractAddress"].as_str().unwrap().parse().unwrap();
    let code = node.result("eth_getCode", json!([format!("{contract:#x}"), "latest"]));
    assert_eq!(code, json!(fixture["deployedBytecode"]));

    // count() == 0, then increment() emits one log.
    let count_call = json!({ "to": format!("{contract:#x}"), "data": format!("0x{}", hex::encode(selector("count()"))) });
    assert_eq!(hex_u64(&node.result("eth_call", json!([count_call, "latest"]))), 0);
    let receipt = send(&node, Some(contract), selector("increment()").to_vec(), 0);
    assert_eq!(receipt["status"], json!("0x1"));
    assert_eq!(receipt["logs"].as_array().unwrap().len(), 1);
    let topic0 = format!("{:#x}", keccak256("Incremented(address,uint256)"));
    assert_eq!(receipt["logs"][0]["topics"][0], json!(topic0));
    assert_eq!(hex_u64(&node.result("eth_call", json!([count_call, "latest"]))), 1);

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
    assert!(error["data"].as_str().unwrap().starts_with(&expected_selector));

    // Snapshot, mutate, revert: count, block number and nonce all go back.
    let snapshot = node.result("evm_snapshot", json!([]));
    assert_eq!(snapshot, json!("0x0"));
    send(&node, Some(contract), selector("increment()").to_vec(), 0);
    assert_eq!(hex_u64(&node.result("eth_call", json!([count_call, "latest"]))), 2);
    assert_eq!(node.result("eth_blockNumber", json!([])), json!("0x3"));
    assert_eq!(node.result("evm_revert", json!([snapshot])), json!(true));
    assert_eq!(hex_u64(&node.result("eth_call", json!([count_call, "latest"]))), 1);
    assert_eq!(node.result("eth_blockNumber", json!([])), json!("0x2"));
    assert_eq!(node.result("eth_getTransactionCount", json!([SENDER, "latest"])), json!("0x2"));
    assert_eq!(node.result("evm_revert", json!([snapshot])), json!(false), "consumed");

    // Time travel: +3600 s then an empty block.
    let before = hex_u64(&node.result("eth_getBlockByNumber", json!(["latest", false]))["timestamp"]);
    assert_eq!(node.result("evm_increaseTime", json!([3600])), json!(3600));
    assert_eq!(node.result("evm_mine", json!([])), json!("0x0"));
    let after = hex_u64(&node.result("eth_getBlockByNumber", json!(["latest", false]))["timestamp"]);
    assert!(after >= before + 3600, "{after} < {before} + 3600");

    // Fees: sender paid gas * price; the fee collector 0.0.98 holds it.
    let paid = balance_before - hex_u256(&node.result("eth_getBalance", json!([SENDER, "latest"])));
    let collector = hex_u256(&node.result("eth_getBalance", json!(["0x0000000000000000000000000000000000000062", "latest"])));
    assert_eq!(paid, collector, "fees are collected, not burned");
    assert_eq!(paid % U256::from(WEIBAR_PER_TINYBAR), U256::ZERO, "whole tinybar");
}

#[test]
fn value_transfer_and_hollow_account() {
    let node = Node::boot();
    let fresh: Address = "0x1234567890123456789012345678901234567890".parse().unwrap();
    let receipt = send(&node, Some(fresh), Vec::new(), 100_000_000);
    assert_eq!(receipt["status"], json!("0x1"));
    assert_eq!(receipt["gasUsed"], json!("0x5208"));
    let balance = hex_u256(&node.result("eth_getBalance", json!([format!("{fresh:#x}"), "latest"])));
    assert_eq!(balance, U256::from(100_000_000u64) * U256::from(WEIBAR_PER_TINYBAR));
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
    let refused = node.error("eth_sendTransaction", json!([{ "from": ghost, "to": SENDER, "value": "0x0" }]));
    assert_eq!(refused["code"], json!(-32000));
    assert!(refused["message"].as_str().unwrap().contains("anvil_impersonateAccount"));

    node.result("anvil_impersonateAccount", json!([ghost]));
    node.result("anvil_setBalance", json!([ghost, "0x21e19e0c9bab2400000"]));
    let hash = node.result("eth_sendTransaction", json!([{ "from": ghost, "to": SENDER, "value": "0x2540be400", "gas": "0x5208" }]));
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
    assert!(error["message"].as_str().unwrap().contains("historical state"));
    assert_eq!(node.result("eth_getBlockByNumber", json!(["0x999", false])), Value::Null);
    let past_hash = B256::ZERO;
    assert_eq!(node.result("eth_getBlockByHash", json!([format!("{past_hash:#x}"), false])), Value::Null);
}
