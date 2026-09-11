#!/usr/bin/env bash
# Calls sweep() on the contract the seed deployed. Before the deadline this reverts with
# "Deadline: too early"; the phase advances the clock a week first, so it does not.
set -eu

SENDER=0x00000000000000000000000000000000000003ea
SWEEP=0x35faa416   # keccak("sweep()")[0..4]
address=$(cat .harness/deadline-address)

curl -s -X POST "$HANVIL_RPC_URL" -H 'content-type: application/json' \
  -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"eth_sendTransaction\",\"params\":[{\"from\":\"$SENDER\",\"to\":\"$address\",\"data\":\"$SWEEP\"}]}" \
  > /dev/null
echo "sweep() sent to $address"
