# hanvil

A local Hedera network that fits in one binary and starts before you finish typing the next
command. It listens where hiero-local-node listens — JSON-RPC on 7546, the mirror REST API on
5551, HAPI gRPC on 50211 — and hands out the same thirty dev accounts with the same ids and keys,
so `@hiero-ledger/sdk`, viem, hardhat and foundry don't know the difference. Unlike the Docker
stack, it can take a snapshot of the whole chain and put it back.

I built it so hedera-harness could run its on-chain validation tier without a testnet account,
without HBAR, and with a clean chain for every repair attempt.

It's early. Today it boots the accounts and answers `eth_chainId`, `eth_blockNumber` and
`eth_getBalance`. The rest of the week's work is laid out in `docs/code-plan.md`; every claim
about how Hedera's own tooling behaves is pinned to a file and line in `docs/research.md`.

```
cargo run --release
curl -s localhost:7546 -d '{"jsonrpc":"2.0","id":1,"method":"eth_getBalance","params":["0x67D8d32E9Bf1a9968a5ff53B87d777Aa8EBBEe69","latest"]}'
```

MIT. Vendored HAPI protobufs are Apache-2.0 — see NOTICE.
