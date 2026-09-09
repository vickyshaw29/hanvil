# hanvil

A local Hedera network in one binary. It boots in 1 ms, prints thirty pre-funded accounts, and
serves the three protocols a Hedera app already speaks — JSON-RPC on 7546, mirror node REST on
5551, HAPI gRPC on 50211 — from one in-memory chain, on the ports `hiero-local-node` uses.
`@hiero-ledger/sdk`, viem, hardhat and foundry connect to it unchanged. Unlike the Docker stack
it can snapshot the whole chain and put it back.

I built it so `hedera-harness` can run its on-chain validation tier without a testnet account,
without HBAR, and with a clean chain for every repair attempt.

## Measured

On an M-series Mac, 2026-09-09, release build, median of five runs:

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

## Flags

```
--port 7546            --mirror-port 5551      --grpc-port 50211
--host 127.0.0.1       --chain-id 298          --accounts 10
--balance 10000        --gas-price 71          --no-sig-verify
--block-time SECONDS   --state FILE            --dump-state FILE
--silent
```

`--state FILE` reads the chain at boot if the file is there and writes it back on exit, so a
restart continues where the last run stopped; the file decides the chain id and the accounts, and
the genesis flags are ignored. `--dump-state FILE` writes without reading. `--block-time SECONDS`
mines an empty block on that interval — transactions still mine their own block the moment they
arrive, so this makes time move, it does not batch.

## Endpoints

| Port | What | Surface |
| --- | --- | --- |
| 7546 | JSON-RPC, relay shape | `eth_chainId` `eth_blockNumber` `eth_getBalance` `eth_getCode` `eth_getStorageAt` `eth_getTransactionCount` `eth_gasPrice` `eth_maxPriorityFeePerGas` `eth_feeHistory` `eth_call` `eth_estimateGas` `eth_sendRawTransaction` `eth_sendTransaction` `eth_getTransactionByHash` `eth_getTransactionReceipt` `eth_getBlockBy{Number,Hash}` `eth_getBlockReceipts` `eth_getLogs` `eth_getBlockTransactionCountBy{Hash,Number}` `eth_getTransactionByBlock{Hash,Number}AndIndex` `net_version` `net_listening` `web3_clientVersion` `web3_sha3` |
| 7546 | Anvil cheats | `evm_snapshot` `evm_revert` `evm_mine` `evm_increaseTime` `evm_setNextBlockTimestamp` `anvil_setBalance` `anvil_setCode` `anvil_setNonce` `anvil_setStorageAt` `anvil_impersonateAccount` `anvil_stopImpersonatingAccount` `anvil_mine` `anvil_nodeInfo`, and the `hardhat_` aliases |
| 5551 | Mirror node REST | `/api/v1/accounts/{id\|alias\|evm}` `/accounts/{id}/tokens` `/transactions` (`account.id` `transactiontype` `result` `timestamp` `limit` `order`) `/transactions/{0.0.x-sss-nnn}` `/contracts/{id\|address}` `/contracts/{id}/results` `/contracts/results/{hash\|txId}` `/contracts/results/logs` `/topics/{id}` `/topics/{id}/messages` `/topics/{id}/messages/{n}` `/blocks` `/blocks/{number\|hash}` `/network/nodes` `/network/fees` `/network/exchangerate` |
| 50211 | HAPI gRPC | `CryptoService`: `createAccount` `cryptoTransfer` `cryptoDelete` `cryptoGetBalance` `getAccountInfo` `getTransactionReceipts` `getTxRecordByTxID`. `ConsensusService`: `createTopic` `submitMessage` `getTopicInfo`. `SmartContractService`: `callEthereum` `contractCallLocalMethod`. `NetworkService`: `getVersionInfo`. `FileService`, `TokenService`, `ScheduleService`, `FreezeService`, `UtilService` and `AddressBookService` are routed and answer `NOT_SUPPORTED` |

## What is emulated, and what is not

The EVM runs in tinybar, as it does on Hedera. One EVM wei is one tinybar; `eth_getBalance` and
`eth_gasPrice` multiply by 10¹⁰ at the JSON-RPC boundary, a `value` that is not a whole number of
tinybar is refused with the relay's error, and a Solidity `1 ether` literal is 10¹⁸ tinybar — the
same quirk real Hedera has. Gas costs 71 tinybar; the fee goes to 0.0.98 instead of being burned,
so supply is conserved and fees are visible on that account. Unused gas is refunded in full —
HIP-1249 removed Hedera's 80 % minimum charge in consensus node 0.69.0, and `hiero-local-node`
pins 0.72.0, so a caller pays for the gas it used, as on Ethereum. A transaction asking for more
than 15,000,000 gas is refused with the relay's `-32005` and its wording, and an `eth_call` asking
for more is capped to it — the relay's `MAX_TRANSACTION_GAS_LIMIT`, so a contract that deploys
here also deploys on Hedera. One block is mined per transaction.
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
  `NOT_SUPPORTED`, and a query Hanvil does not answer comes back as a gRPC status naming it.
  `FileService`, `TokenService`, `ScheduleService`, `FreezeService`, `UtilService` and
  `AddressBookService` are registered for that reason alone: an unregistered service is not
  routed, and tonic would answer a call to one with a bare `12 UNIMPLEMENTED` the SDK reports as
  a transport failure rather than a refusal the network made. `ContractCreateFlow` deploys
  through `FileService`, so it is refused rather than hung.
- The alias key's signature on `cryptoCreateAccount`, and `receiverSigRequired`. The payer's
  signature is what a create is checked against.
- HTS, HFS, scheduled transactions, token and NFT data. `/accounts/{id}/tokens` is always empty.
  A call to a Hedera system contract reverts rather than pretending — see
  [System contracts](#system-contracts) below.
- Chunked topic messages keep no chunk metadata: each chunk is stored as its own message, and
  the mirror's `chunk_info` is null.
- `AccountInfo.alias` over HAPI is empty for the same reason the mirror's is null; the EVM
  address is in `contractAccountId`.
- Historical state. Only the head is served; asking for an older block is an error, not a guess.
- The mirror's query filters outside the ones listed in the endpoint table. `timestamp` and
  `transactiontype` off `/transactions`, `block.number`, `topic0`–`topic3`, `hbar`, `nonce`,
  `scheduled`, `type`, `internal`, `from`, `encoding`, `file_id` and `node.id` are refused with
  `400 Invalid parameter` naming what the endpoint does apply. A real mirror would filter on them;
  answering 200 with the unfiltered list is the one wrong answer a caller cannot detect.
- Pagination. `links.next` is always null and a list is cut at `limit` (default 25, maximum 100).
  A query with more matches than the limit returns the first page and no cursor to the rest.
- Batch mining: `evm_setAutomine` and `evm_setIntervalMining` return `-32601` with the reason.
- `anvil_dumpState`, `anvil_loadState` and `anvil_reset` over JSON-RPC. State does persist across
  restarts, through `--state` / `--dump-state` on the command line.
- The mirror's `alias` field is null — Hanvil mints EVM-address aliases, which `evm_address`
  already carries, not base32 key aliases.
- A block's `hash` is a 32-byte keccak over its own fields, not a 48-byte record file hash, and
  `hapi_version` is null because Hanvil is not a consensus node and will not claim a version.
- A transaction's `transfers` list carries the fee and the top-level value transfer. Value moved
  by an inner call is not itemised.
- The exchange rate is fixed at 1 ℏ = 12 ¢ and never expires.
- Key lists and threshold keys. Accounts hold one key.
- The mirror node's gRPC API on port 5600. `Client.forLocalNode()` points its mirror network
  there, so `TopicMessageQuery` finds nothing listening and retries twenty times before giving up.
  Read topic messages over REST — `GET /api/v1/topics/{id}/messages` — or point the SDK's mirror
  network at a real one.
- The relay's WebSocket endpoint on port 8546. `eth_subscribe` and log watching over WS have
  nothing to connect to; poll `eth_getLogs` instead.
- Forking testnet or mainnet state. Hanvil starts from its own genesis every time and makes no
  outbound calls; `--state` replays a file Hanvil itself wrote.

### System contracts

Hedera puts three system contracts at fixed addresses in the EVM. Hanvil emulates none of them:

| Entity | Address | What it is |
| --- | --- | --- |
| `0.0.359` | `0x…0167` | Hedera Token Service |
| `0.0.360` | `0x…0168` | exchange rate, `tinycentsToTinybars(uint256)` |
| `0.0.361` | `0x…0169` | pseudorandom seed, `getPseudorandomSeed()` (HIP-351) |

An address with no code is not an error in the EVM: a call to one succeeds and returns nothing.
A contract that calls `createFungibleToken` on an unemulated chain would therefore read the call
as having worked and carry on with a token that does not exist, and one that asks `0x…0169` for a
random seed would take the zero it did not get as the seed. So genesis etches bytecode at all
three that reverts with `Error(string)`:

```
hanvil: HTS system contract not emulated; see README#system-contracts
hanvil: exchange rate system contract not emulated; see README#system-contracts
hanvil: PRNG system contract not emulated; see README#system-contracts
```

viem, ethers and `cast` all decode that, and `eth_call` returns it as `{"code": 3, "data": "0x08c379a0…"}`.
`anvil_setCode` overwrites it if you want to put a mock there.

The Hedera Account Service (HIP-632) is not stubbed. HIP-632 names its functions and not its
address, and Hanvil does not etch an address it cannot cite.

Etched bytecode, not a precompile: the behaviour a caller sees is identical, and it survives
`evm_snapshot` / `evm_revert` because it is part of the chain state that gets cloned.

## Harness integration

`hedera-harness` runs its on-chain validation tier against Hanvil with no credentials. Measured
2026-09-08, five runs, 3.6-4.3 s wall clock:

```
[hedera-harness] Chain signer provisioned - 0.0.1033 (0x61a73ab7...)
[hedera-harness] Attempt 1 PASSED - deterministic gates passed
[hedera-harness] Chain signer swept - 0.0.1033
Run PASSED
```

The recipe is four lines and names no operator:

```yaml
chainValidation:
  enabled: true
  network: local
```

Nothing is set in the environment - no `HEDERA_OPERATOR_ID`, no `HEDERA_OPERATOR_KEY`. The run
artifacts carry `{"type":"chain_signer_provisioned","network":"local"}` and
`{"type":"chain_signer_swept","success":true}`, and `GET /api/v1/accounts/0.0.1033` on the mirror
shows the account created and then deleted. To reproduce, with the harness branch below built:

```
./target/release/hanvil &
cp -R tests/harness /tmp/project && cd /tmp/project && git init -q -b main . && git add -A && git commit -qm fixture
node <harness>/dist/index.js doctor .harness/spec.yaml
node <harness>/dist/index.js run   .harness/spec.yaml
```

`.github/workflows/ci.yml` runs exactly that on every push, with no secrets, and asserts the
signer reached `network: local` rather than trusting the verdict.

`network: "local"` is not in `hedera-harness` yet. It is two open PRs against
`hedera-dev/hedera-harness` `dev`:

| PR | What it does |
| --- | --- |
| [#47](https://github.com/hedera-dev/hedera-harness/pull/47) | `network: "local"` for the signer, `doctor` and the validator prompt |
| [#48](https://github.com/hedera-dev/hedera-harness/pull/48) | `evm_snapshot` before an attempt, `evm_revert` after a failed one |

Without the second, a repair attempt inherits whatever the previous attempt wrote on chain. On a
recipe that mines three blocks per attempt and then fails, attempts end at block `0x3`, `0x6`,
`0x9`; with it, `0x3`, `0x3`, `0x3`. #48 stacks on #47 and contains its commits; CI builds the
harness from that branch.

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
