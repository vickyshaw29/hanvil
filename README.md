# hanvil

A local Hedera network that fits in one binary and starts before you finish typing the next
command. It listens where hiero-local-node listens — JSON-RPC on 7546, the mirror REST API on
5551, HAPI gRPC on 50211 — and hands out the same thirty dev accounts with the same ids and keys,
so `@hiero-ledger/sdk`, viem, hardhat and foundry don't know the difference. Unlike the Docker
stack, it can take a snapshot of the whole chain and put it back.

I built it so hedera-harness could run its on-chain validation tier without a testnet account,
without HBAR, and with a clean chain for every repair attempt.

It's early. Today the JSON-RPC side works end to end: `cast send --create` deploys a contract,
viem writes to it and decodes its custom errors, `eth_getLogs` finds the events, and
`evm_snapshot` / `evm_revert` put the whole chain back — state, blocks, nonces, clock. The mirror
REST and gRPC listeners are next. The week's work is laid out in `docs/code-plan.md`; every claim
about how Hedera's own tooling behaves is pinned to a file and line in `docs/research.md`.

```
cargo run --release
cast send --rpc-url localhost:7546 --private-key 0x105d050185ccb907fba04dd92d8de9e32c18305e097ab41dadda21489a211524 --create 0x6080...
cast rpc --rpc-url localhost:7546 evm_snapshot
```

The EVM runs in tinybar, as it does on Hedera: `eth_getBalance` reports 18 decimals, a `value`
that is not a whole number of tinybar is refused, and a Solidity `1 ether` is 10^18 tinybar.
Fees go to 0.0.98 instead of being burned. Only the head state is served; ask for an older block
and you get an error, not a guess.

MIT. Vendored HAPI protobufs are Apache-2.0 — see NOTICE.
