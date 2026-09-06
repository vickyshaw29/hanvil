# hanvil

Anvil for Hedera. One binary, one in-memory chain, three listeners: JSON-RPC on 7546 in the
shape of the Hedera JSON-RPC relay, mirror-node REST on 5551, HAPI gRPC on 50211. Tooling built
for hiero-local-node — `@hiero-ledger/sdk`, viem, hardhat, foundry, hedera-harness — connects
without changes. Same predefined accounts, same ids, same ports.

Day 0. Boots with the thirty hiero-local-node dev accounts and answers `eth_chainId`,
`eth_blockNumber`, `eth_getBalance`. Everything else lands this week; the plan is in
`docs/code-plan.md` and every upstream fact it rests on is in `docs/research.md`.

```
cargo run --release
curl -s localhost:7546 -d '{"jsonrpc":"2.0","id":1,"method":"eth_getBalance","params":["0x67D8d32E9Bf1a9968a5ff53B87d777Aa8EBBEe69","latest"]}'
```

MIT. Vendored HAPI protobufs are Apache-2.0 — see NOTICE.
