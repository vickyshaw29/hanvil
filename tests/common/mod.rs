//! The harness the integration tests share: boot the binary on random ports, read the ports off
//! its banner, and talk to it. No HTTP client crate — a request is one HTTP/1.1 exchange over a
//! TcpStream, which is also what proves the listeners speak plain HTTP/1.1.
#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use alloy_consensus::{SignableTransaction as _, TxEnvelope, TxLegacy};
use alloy_eips::eip2718::Encodable2718 as _;
use alloy_primitives::{Address, Bytes, Signature, TxKind, U256, keccak256};
use serde_json::{Value, json};

/// 0.0.1012, the first alias account: hiero-local-node's README lists it first for hardhat.
pub const KEY: &str = "0x105d050185ccb907fba04dd92d8de9e32c18305e097ab41dadda21489a211524";
pub const SENDER: &str = "0x67d8d32e9bf1a9968a5ff53b87d777aa8ebbee69";
pub const WEIBAR_PER_TINYBAR: u128 = 10_000_000_000;

pub struct Node {
    child: Child,
    /// JSON-RPC port, from the banner.
    pub port: u16,
    /// Mirror REST port, from the banner.
    pub mirror_port: u16,
}

impl Node {
    pub fn boot() -> Self {
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
        let mut mirror_port = None;
        while Instant::now() < deadline {
            let Some(Ok(line)) = lines.next() else { break };
            if let Some(rest) = line.strip_prefix("JSON-RPC   http://127.0.0.1:") {
                port = rest.split_whitespace().next().and_then(|p| p.parse().ok());
            }
            if let Some(rest) = line.strip_prefix("Mirror     http://127.0.0.1:") {
                mirror_port = rest.split('/').next().and_then(|p| p.trim().parse().ok());
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
            mirror_port: mirror_port.expect("banner names the bound mirror port"),
        }
    }

    pub fn rpc(&self, method: &str, params: Value) -> Value {
        let body =
            json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }).to_string();
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

    /// `GET path` against the mirror listener: the status code and the JSON body.
    pub fn get(&self, path: &str) -> (u16, Value) {
        let mut stream = TcpStream::connect(("127.0.0.1", self.mirror_port)).expect("connect");
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
        )
        .expect("write request");
        let mut response = String::new();
        stream.read_to_string(&mut response).expect("read response");
        let (headers, payload) = response.split_once("\r\n\r\n").expect("http body");
        let status = headers
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .expect("status line");
        (status, serde_json::from_str(payload).expect("json body"))
    }

    pub fn result(&self, method: &str, params: Value) -> Value {
        let response = self.rpc(method, params);
        assert!(
            response.get("error").is_none(),
            "{method} failed: {}",
            response["error"]
        );
        response["result"].clone()
    }

    pub fn error(&self, method: &str, params: Value) -> Value {
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

pub fn hex_u64(value: &Value) -> u64 {
    let text = value.as_str().expect("hex quantity");
    u64::from_str_radix(text.trim_start_matches("0x"), 16).expect("parses")
}

pub fn hex_u256(value: &Value) -> U256 {
    let text = value.as_str().expect("hex quantity");
    U256::from_str_radix(text.trim_start_matches("0x"), 16).expect("parses")
}

pub fn counter_fixture() -> Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/Counter.json");
    serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("json")
}

pub fn selector(signature: &str) -> [u8; 4] {
    let hash = keccak256(signature.as_bytes());
    [hash[0], hash[1], hash[2], hash[3]]
}

/// Sign a legacy transaction with the dev key, with the gas limit `eth_estimateGas` reports.
pub fn sign_legacy(node: &Node, to: Option<Address>, input: Vec<u8>, value_tinybar: u64) -> String {
    let mut call = json!({ "from": SENDER, "data": format!("0x{}", hex::encode(&input)) });
    if let Some(to) = to {
        call["to"] = json!(format!("{to:#x}"));
    }
    if value_tinybar > 0 {
        call["value"] = json!(format!(
            "0x{:x}",
            u128::from(value_tinybar) * WEIBAR_PER_TINYBAR
        ));
    }
    let gas = hex_u64(&node.result("eth_estimateGas", json!([call])));
    sign_legacy_with_gas(node, to, input, value_tinybar, gas)
}

/// Sign a legacy transaction with an explicit gas limit. A transaction that reverts cannot be
/// estimated — `eth_estimateGas` reports the revert — so a test that wants one mined names the gas.
pub fn sign_legacy_with_gas(
    node: &Node,
    to: Option<Address>,
    input: Vec<u8>,
    value_tinybar: u64,
    gas: u64,
) -> String {
    let nonce = hex_u64(&node.result("eth_getTransactionCount", json!([SENDER, "latest"])));
    let gas_price = hex_u256(&node.result("eth_gasPrice", json!([])));
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
    format!(
        "0x{}",
        hex::encode(TxEnvelope::Legacy(signed).encoded_2718())
    )
}

pub fn send(node: &Node, to: Option<Address>, input: Vec<u8>, value_tinybar: u64) -> Value {
    let raw = sign_legacy(node, to, input, value_tinybar);
    submit(node, raw)
}

/// Mine a transaction that is expected to revert, with an explicit gas limit.
pub fn send_with_gas(
    node: &Node,
    to: Option<Address>,
    input: Vec<u8>,
    value_tinybar: u64,
    gas: u64,
) -> Value {
    let raw = sign_legacy_with_gas(node, to, input, value_tinybar, gas);
    submit(node, raw)
}

fn submit(node: &Node, raw: String) -> Value {
    let hash = node.result("eth_sendRawTransaction", json!([raw]));
    let receipt = node.result("eth_getTransactionReceipt", json!([hash]));
    assert!(!receipt.is_null(), "receipt is available immediately");
    receipt
}
