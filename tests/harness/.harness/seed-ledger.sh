#!/usr/bin/env bash
# Three transfers from a predefined account: one the node takes, two it refuses because one
# weibar is not a whole tinybar. The refused pair is what a mirror node has no row for, and
# what the chain ledger exists to show.
set -eu

send() {
  curl -s -X POST "$HANVIL_RPC_URL" -H 'content-type: application/json' \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"eth_sendTransaction\",\"params\":[{\"from\":\"0x00000000000000000000000000000000000003ea\",\"to\":\"0x0000000000000000000000000000000000000001\",\"value\":\"$1\"}]}" \
    > /dev/null
}

send 0x2540be400   # 1 tinybar, accepted
send 0x1           # 1 weibar, refused
send 0x1           # 1 weibar, refused
