# hanvil

A local Hedera network in one binary. It boots in 1 ms, prints thirty pre-funded accounts, and
serves the three protocols a Hedera app already speaks — JSON-RPC on 7546, mirror node REST on
5551, HAPI gRPC on 50211 — from one in-memory chain, on the ports `hiero-local-node` uses.
`@hiero-ledger/sdk`, viem, hardhat and foundry connect to it unchanged. Unlike the Docker stack
it can snapshot the whole chain and put it back.

I built it so `hedera-harness` can run its on-chain validation tier without a testnet account,
without HBAR, and with a clean chain for every repair attempt.

## Measured

On an M-series Mac, 2026-09-08, release build, median of five runs:

| | hanvil | how it was measured |
| --- | --- | --- |
| Boot to listeners bound | 1 ms | the binary prints `Started in 1 ms` |
| Resident memory | 4.2 MB | `ps -o rss= -p $(pgrep -x hanvil)` |
| Binary | 7.1 MB | `ls -l target/release/hanvil` |
| Accounts pre-funded | 30, 10,000 ℏ each | the boot banner |

CI asserts the median boot stays under 100 ms on ubuntu and macos runners
(`.github/workflows/ci.yml`). The comparison against `hiero-local-node` is not measured yet, so
this table does not carry it.

## Run it

```
cargo build --release
./target/release/hanvil
curl -s localhost:5551/api/v1/accounts/0.0.1012 | jq .balance
```

```
hanvil 0.1.0 — local Hedera network
JSON-RPC   http://127.0.0.1:7546   chain id 298
Mirror     http://127.0.0.1:5551/api/v1
HAPI gRPC  127.0.0.1:50211          node 0.0.3

Accounts (ECDSA, long-zero address)
0.0.1002  0x00000000000000000000000000000000000003ea  0x7f109a9e3b0d8ecfba9cc23a3614433ce0fa7ddcc80f2a8f10b222179a5a80d6
…
Accounts (ECDSA with EVM alias)
0.0.1012  0x67D8d32E9Bf1a9968a5ff53B87d777Aa8EBBEe69  0x105d050185ccb907fba04dd92d8de9e32c18305e097ab41dadda21489a211524
…
Started in 1 ms
```

The accounts, their ids and their keys are `hiero-local-node`'s, byte for byte, so anything
configured for it works unchanged. They are development keys; they must never hold value.

## Endpoints

| Port | What | Surface |
| --- | --- | --- |
| 7546 | JSON-RPC, relay shape | `eth_chainId` `eth_blockNumber` `eth_getBalance` `eth_getCode` `eth_getStorageAt` `eth_getTransactionCount` `eth_gasPrice` `eth_maxPriorityFeePerGas` `eth_feeHistory` `eth_call` `eth_estimateGas` `eth_sendRawTransaction` `eth_sendTransaction` `eth_getTransactionByHash` `eth_getTransactionReceipt` `eth_getBlockBy{Number,Hash}` `eth_getBlockReceipts` `eth_getLogs` `eth_getBlockTransactionCountBy{Hash,Number}` `eth_getTransactionByBlock{Hash,Number}AndIndex` `net_version` `net_listening` `web3_clientVersion` `web3_sha3` |
| 7546 | Anvil cheats | `evm_snapshot` `evm_revert` `evm_mine` `evm_increaseTime` `evm_setNextBlockTimestamp` `anvil_setBalance` `anvil_setCode` `anvil_setNonce` `anvil_setStorageAt` `anvil_impersonateAccount` `anvil_stopImpersonatingAccount` `anvil_mine` `anvil_nodeInfo`, and the `hardhat_` aliases |
| 5551 | Mirror node REST | `/api/v1/accounts/{id\|alias\|evm}` `/accounts/{id}/tokens` `/transactions` `/transactions/{0.0.x-sss-nnn}` `/contracts/{id\|address}` `/contracts/{id}/results` `/contracts/results/{hash\|txId}` `/contracts/results/logs` `/topics/{id}` `/topics/{id}/messages` `/topics/{id}/messages/{n}` `/blocks` `/blocks/{number\|hash}` `/network/nodes` `/network/fees` `/network/exchangerate` |
| 50211 | HAPI gRPC | `CryptoService`: `createAccount` `cryptoTransfer` `cryptoDelete` `cryptoGetBalance` `getAccountInfo` `getTransactionReceipts` `getTxRecordByTxID`. `ConsensusService`: `createTopic` `submitMessage` `getTopicInfo`. `SmartContractService`: `callEthereum` `contractCallLocalMethod`. `NetworkService`: `getVersionInfo` |

## What is emulated, and what is not

The EVM runs in tinybar, as it does on Hedera. One EVM wei is one tinybar; `eth_getBalance` and
`eth_gasPrice` multiply by 10¹⁰ at the JSON-RPC boundary, a `value` that is not a whole number of
tinybar is refused with the relay's error, and a Solidity `1 ether` literal is 10¹⁸ tinybar — the
same quirk real Hedera has. Gas costs 71 tinybar; the fee goes to 0.0.98 instead of being burned,
so supply is conserved and fees are visible on that account. One block is mined per transaction.
Contracts created through the EVM are allocated a `0.0.N` id and appear on the mirror endpoints
under it.

HAPI transactions run the prechecks a node runs, in the same order and with the same
`ResponseCodeEnum` numbers: node account, transaction id, valid duration, valid start, duplicate
id, payer, supported body, signatures, payer balance. A body that passes precheck but fails at
consensus still leaves a record and still costs the fee. Signatures are checked for real — ECDSA
over `keccak256(bodyBytes)`, ED25519 over `bodyBytes` — for the payer, for every account a
transfer debits, for the account a delete removes, and for a topic's submit key. `--no-sig-verify`
turns that off and leaves every other check running. Topic running hashes are SHA-384 over the
version 3 input list from `transaction_receipt.proto`.

Not emulated. Each of these is a deliberate hole, not an oversight:

- Consensus, gossip, multiple nodes, staking, record files.
- The fee schedule. Every HAPI transaction costs a flat 10,000 tinybar whatever its body; every
  EVM transaction costs gas at 71 tinybar. A query costs nothing, and `COST_ANSWER` says so.
- HAPI bodies other than `cryptoCreateAccount`, `cryptoTransfer`, `cryptoDelete`,
  `consensusCreateTopic`, `consensusSubmitMessage` and `ethereumTransaction`. The rest answer
  `NOT_SUPPORTED` naming the surface that does work. `FileService`, `TokenService`,
  `ScheduleService` and `FreezeService` are not registered at all.
- The alias key's signature on `cryptoCreateAccount`, and `receiverSigRequired`. The payer's
  signature is what a create is checked against.
- HTS, HFS, scheduled transactions, token and NFT data. `/accounts/{id}/tokens` is always empty.
- Chunked topic messages keep no chunk metadata: each chunk is stored as its own message, and
  the mirror's `chunk_info` is null.
- `AccountInfo.alias` over HAPI is empty for the same reason the mirror's is null; the EVM
  address is in `contractAccountId`.
- Historical state. Only the head is served; asking for an older block is an error, not a guess.
- Batch mining: `evm_setAutomine` and `evm_setIntervalMining` return `-32601` with the reason.
- `anvil_dumpState`, `anvil_loadState`, `anvil_reset`, and `--state` persistence across restarts.
- The mirror's `alias` field is null — Hanvil mints EVM-address aliases, which `evm_address`
  already carries, not base32 key aliases.
- A block's `hash` is a 32-byte keccak over its own fields, not a 48-byte record file hash, and
  `hapi_version` is null because Hanvil is not a consensus node and will not claim a version.
- A transaction's `transfers` list carries the fee and the top-level value transfer. Value moved
  by an inner call is not itemised.
- The exchange rate is fixed at 1 ℏ = 12 ¢ and never expires.
- Key lists and threshold keys. Accounts hold one key.

## Harness integration

Two PRs against `hedera-dev/hedera-harness` `dev` make its Tier 3.5 chain validation run here:
`network: "local"` for the signer and the validator prompt, and a snapshot per repair attempt so
retries do not inherit the previous attempt's on-chain state. Neither is open yet.

## How it is built

One `Chain` struct behind one `RwLock`. Every listener takes the same lock, so a transfer over
JSON-RPC is visible to the mirror in the same millisecond. `evm_snapshot` clones the struct;
`evm_revert` swaps it back.

```
 JSON-RPC :7546 ─┐
 mirror   :5551 ─┼─→ RwLock<Chain> ─→ accounts · blocks · receipts · logs · topics ·
 HAPI     :50211 ┘                    HAPI records · revm CacheDB
                                      (balances authoritative in tinybar)
```

`revm` executes, `alloy` decodes and recovers senders, `axum` serves all three listeners, `tonic`
and `prost` speak HAPI over the 130 vendored protobuf files, `clap` reads the flags. No outbound network calls — the binary never fetches
anything. The build is laid out in `docs/code-plan.md`; every claim about how Hedera's own tooling
behaves is pinned to a file and line in `docs/research.md`.

MIT. Vendored HAPI protobufs are Apache-2.0 — see NOTICE.
