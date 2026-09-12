# hanvil

hanvil is a local Hedera network that runs as a single process.

It serves the three protocols a Hedera app already speaks — JSON-RPC on 7546, mirror node REST on
5551, HAPI gRPC on 50211 — from one in-memory chain, on the ports and with the accounts
`hiero-local-node` uses. The same binary is also a coding-agent harness (`hanvil run`) and an
[x402](https://x402.org) payment rail (`hanvil toll`) that work against that chain.

```
$ hanvil
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
Started in 2 ms
```

**[Measured](#measured)** · [Quickstart](#quickstart) · [Endpoints](#endpoints) ·
[What is not emulated](#what-is-emulated-and-what-is-not) · [The harness](#hanvil-run--the-harness) ·
[Toll](#hanvil-toll--x402-on-the-local-chain) · [Authorship](#authorship-and-ai-use) ·
[Reference](docs/reference.md)

## Why use it

- hanvil answers its first JSON-RPC call **6 ms** after exec. `hiero-local-node` takes 50.6 s and
  3.8 GB across 18 containers. [The numbers](#measured).
- It uses `hiero-local-node`'s ports, and its thirty pre-funded accounts and keys byte for byte, so
  `@hiero-ledger/sdk`, viem, hardhat and foundry connect unchanged.
- It can snapshot the whole chain and put it back, with `evm_snapshot` and `evm_revert`. It can
  survive a restart, with `--state FILE`. The Docker stack does neither.
- **It keeps the transactions Hedera refuses before consensus.** Those write no record, no receipt
  and no mirror row on any Hedera network, so nothing that reads a mirror node can show them to
  you. hanvil is the node, so it keeps them and serves them as `hanvil_rejections`.
- The EVM runs in tinybar, as it does on Hedera. One wei is one tinybar, and a `value` that is not
  a whole number of tinybar is refused here with the relay's own error.
- A call to an unemulated Hedera system contract reverts with a named reason. It does not silently
  return nothing, which is what a call to an empty address would do.
- `hanvil run` drives a coding agent through a `hedera-harness` schema-v3 recipe against that
  chain: a snapshot before every attempt, a revert after a failed one, chain assertions with no
  mirror round trip. No testnet account, no HBAR, no credentials.
- Recipes can assert on what upstream cannot express: refusals, decoded contract events, and
  phases that move the chain clock a week between assertion sets.
- Any attempt's chain boots again from its state dump, so you can replay the network as an attempt
  left it.
- `hanvil toll` serves an x402 payment rail on the same chain. A paid request settles in 0.081 s,
  with no facilitator to sign up for.
- One binary, 10 MB, one process. The node has no runtime dependency.

## Why you might not

hanvil emulates Hedera; it does not run it. Five headline limits, and
[35 further declared holes](#what-is-emulated-and-what-is-not):

- **It is not a consensus node.** No gossip, no multiple nodes, no staking, no record files. If
  what you are testing is consensus behaviour, run `hiero-local-node`.
- **HTS, HFS and scheduled transactions are absent.** `/accounts/{id}/tokens` is always empty, and
  a call to the HTS system contract reverts rather than pretending.
- **The fee schedule is flat.** Every HAPI transaction costs 10,000 tinybar whatever its body, and
  EVM gas costs 71 tinybar. A query costs nothing.
- **Six HAPI bodies are implemented.** `cryptoCreateAccount`, `cryptoTransfer`, `cryptoDelete`,
  `consensusCreateTopic`, `consensusSubmitMessage`, `ethereumTransaction`. The rest answer
  `NOT_SUPPORTED`.
- **Only head state is served.** No historical blocks, and no forking of testnet or mainnet state.

## Measured

| | hanvil | hiero-local-node | ratio |
| --- | --- | --- | --- |
| Boot to a JSON-RPC answer | **6 ms** | 50.6 s | 8,400× |
| Boot, as each reports it | **1 ms** | 45.9 s | 45,900× |
| Resident memory | **5.8 MB** | 3,898 MB | 672× |
| On disk | **10.1 MB** | 9.4 GB | 930× |
| Processes | **1** | 18 running, 23 created | — |
| Snapshot and restore the chain | `evm_snapshot` / `evm_revert` | not supported | — |
| Chain survives a restart | `--state FILE` | not supported | — |

One machine, one afternoon: an M-series Mac, release build, hanvil re-measured 2026-09-13 and the
Docker stack 2026-09-11. Each hanvil figure is the median of five runs.
[Every command that produced them](docs/reference.md#reproducing-the-numbers).

The last two rows are `hiero-local-node`'s own answer to its own FAQ: *"Can I stop the local node,
save its state then start it again after a while? No, currently the local node doesn't support
network freezing. Once you stop it, the next start will be with a genesis state and all of your
accounts/contracts/tokens will be wiped."*

What the stack buys is not free. It runs the real consensus node, the real mirror node and the real
relay, and hanvil emulates them. Nothing here says the stack is badly built. It says an inner loop
should not cost 50 seconds and 3.8 GB.

It is also being retired. Hedera announced `hiero-local-node`'s deprecation in March 2026 on a
six-month transition that completes this month — "no further updates, bug fixes, or support". The
replacement is [`hiero-ledger/solo`](https://github.com/hiero-ledger/solo), which runs the same
network on Kubernetes: `npm i -g @hiero-ledger/solo` takes 886 s and installs 362 MB over 223
dependencies, a single-node network is 16 pods across 48 container images, and the stated minimum
is 12 GB of RAM and 6 cores. **No Solo boot time was obtained here, and none is quoted.** `solo
one-shot single deploy` failed after 732 s with `SOLO-3035 Failed to create Kubernetes pod`, on a
machine where Docker Desktop's default 7.8 GB is below Solo's own preflight minimum and three
`ghcr.io` pulls timed out; the partial cluster's 2.14 GB is a floor, not a measurement
([docs/research.md](docs/research.md#solo-measured-2026-09-08)). Solo also moves the ports to
37546 / 38081 / 35211. hanvil keeps `hiero-local-node`'s, which is what existing configs and the
SDK's `forLocalNode()` already point at.

hanvil's own numbers, same machine:

| | | how it was measured |
| --- | --- | --- |
| Tests, all green | 225 | `cargo test --release` |
| Accounts pre-funded | 30, 10,000 ℏ each | the boot banner |
| One x402 paid request, settled on hanvil | 0.081 s | `yarn pay` against `hanvil toll`, median of 9 |
| The same request, settled on Hedera testnet | 4.91 s | `TOLL_URL=…up.railway.app yarn pay`, median of 5 |
| `hanvil doctor`, every check | 60 ms | `time hanvil doctor` in a copy of `tests/harness` |
| `hanvil run`, one attempt, fake agent | 0.21 s | `time hanvil run --no-skills` in the same copy |
| Signer provisioned on the chain | 152 µs | `chain_signer_provisioned.durationMicros` |
| Chain snapshot before an attempt | 10 µs | `chain_snapshot_taken.durationMicros` |
| Chain state dump for replay | 35,122 bytes in 133 µs | `chain_state_written` |
| Full run with `claude` on `examples/hcs-receipts-api` | 9 min 32 s, 2 attempts | `report.json.durationMs`, 2026-09-10 |

The last five come from `.harness/runs/harness.log.jsonl`, which records every event with its
timestamp. CI asserts the median boot stays under 100 ms on ubuntu and macos runners.

## Quickstart

```
cargo build --release
./target/release/hanvil
curl -s localhost:5551/api/v1/accounts/0.0.1012 | jq .balance
```

The accounts, their ids and their keys are `hiero-local-node`'s, byte for byte, so anything
configured for it works unchanged. They are development keys. They must never hold value.

```
--port 7546            --mirror-port 5551      --grpc-port 50211
--host 127.0.0.1       --chain-id 298          --accounts 10
--balance 10000        --gas-price 71          --no-sig-verify
--block-time SECONDS   --state FILE            --dump-state FILE
--silent
```

`--state FILE` reads the chain at boot if the file is there and writes it back on exit, so a
restart continues where the last run stopped. The file decides the chain id and the accounts, and
the genesis flags are ignored. `--dump-state FILE` writes without reading. `--block-time SECONDS`
mines an empty block on that interval; transactions still mine their own block the moment they
arrive, so this makes time move rather than batching it.

One caveat on `--mirror-port`: it moves the REST listener, but `@hiero-ledger/sdk` hardcodes `5551`
for a `127.0.0.1` mirror (`hiero-sdk-js/src/MirrorNode.js:59-60`), so the SDK's REST-backed queries
follow the default and not the flag.

## Endpoints

| Port | Protocol | Serves |
| --- | --- | --- |
| 7546 | JSON-RPC, relay shape | 32 `eth_*` / `net_*` / `web3_*` methods, 13 Anvil cheats and their `hardhat_` aliases, plus `hanvil_rejections` |
| 5551 | Mirror node REST | accounts, balances, transactions, contracts, contract results and logs, topics, topic messages, blocks, network |
| 50211 | HAPI gRPC | `CryptoService`, `ConsensusService`, `SmartContractService`, `NetworkService`; six more services routed and answering `NOT_SUPPORTED` |

Every method and path, and the query filters each endpoint applies, are in
[docs/reference.md](docs/reference.md).

## What is emulated, and what is not

**The EVM runs in tinybar.** One EVM wei is one tinybar, exactly as on Hedera. `eth_getBalance`
and `eth_gasPrice` multiply by 10¹⁰ at the JSON-RPC boundary, and a Solidity `1 ether` literal is
10¹⁸ tinybar — the same quirk real Hedera has. Gas costs 71 tinybar and the fee goes to 0.0.98
rather than being burned, so supply is conserved and fees are visible on that account. Unused gas
is refunded in full: HIP-1249 removed Hedera's 80 % minimum charge in consensus node 0.69.0, and
`hiero-local-node` pins 0.72.0. A transaction asking for more than 15,000,000 gas is refused with
the relay's `-32005` and its wording, and an `eth_call` asking for more is capped to it — the
relay's `MAX_TRANSACTION_GAS_LIMIT` — so a contract that deploys here also deploys on Hedera. One block is mined per transaction.

**HAPI transactions run the prechecks a node runs**, in the same order and with the same
`ResponseCodeEnum` numbers: node account, transaction id, valid duration, valid start, duplicate
id, payer, supported body, signatures, payer balance. Signatures are checked for real — ECDSA over
`keccak256(bodyBytes)`, ED25519 over `bodyBytes` — for the payer, for every account a transfer
debits, for the account a delete removes, and for a topic's submit key. `--no-sig-verify` turns
that off and leaves every other check running. Topic running hashes are SHA-384 over the version 3
input list from `transaction_receipt.proto`.

**A transaction that fails precheck leaves no record, no receipt and no mirror row, as on Hedera.**
hanvil keeps it in a list of its own, which `hanvil_rejections` answers over JSON-RPC and
`hanvil run` reads for [the chain ledger](#the-chain-ledger). The HAPI and mirror surfaces are
unchanged, so a client sees exactly what a node would show it; `hanvil_` is a cheat namespace
beside `anvil_`, not a Hedera endpoint.

Not emulated. Each is a deliberate hole, not an oversight:

- **Consensus.** No gossip, no multiple nodes, no staking, no record files.
- **The fee schedule.** A flat 10,000 tinybar per HAPI transaction, 71 tinybar per unit of EVM gas.
  Queries are free, and `COST_ANSWER` says so.
- **HAPI bodies beyond the six implemented.** The rest answer `NOT_SUPPORTED`, and a query hanvil
  does not answer comes back as a gRPC status naming it. `ContractCreateFlow` deploys through
  `FileService`, so it is refused rather than hung.
- **The alias key's signature on `cryptoCreateAccount`, and `receiverSigRequired`.** The payer's
  signature is what a create is checked against.
- **HTS, HFS, scheduled transactions, token and NFT data.** `/accounts/{id}/tokens` is always
  empty.
- **Chunk metadata on topic messages.** Each chunk is stored as its own message and the mirror's
  `chunk_info` is null.
- **`AccountInfo.alias` over HAPI**, which is empty for the same reason the mirror's is null. The
  EVM address is in `contractAccountId`.
- **Historical state.** Only the head is served. Asking for an older block is an error, not a
  guess.
- **Mirror query filters outside the documented set.** `timestamp` and `transactiontype` off
  `/transactions`, `block.number`, `topic0`–`topic3`, `hbar`, `nonce`, `scheduled`, `type`,
  `internal`, `from`, `encoding`, `file_id` and `node.id` are refused with `400 Invalid parameter`
  naming what the endpoint does apply. Answering 200 with an unfiltered list is the one wrong
  answer a caller cannot detect.
- **Pagination.** `links.next` is always null and a list is cut at `limit` (default 25, maximum
  100). A query with more matches returns the first page and no cursor to the rest.
- **Batch mining.** `evm_setAutomine` and `evm_setIntervalMining` return `-32601` with the reason.
- **A transaction pool.** A transaction whose nonce is above the sender's is refused, not held
  until the gap fills — Hedera refuses a future nonce too. A client that fires transactions in
  parallel without awaiting receipts, which works on Anvil, gets every one after the first back as
  `nonce too high`. Send them in nonce order, or await each receipt.
- **HAPI size limits beyond two.** The serialised transaction is capped at 6,144 bytes
  (`TRANSACTION_OVERSIZE`) and the memo at 100 bytes (`MEMO_TOO_LONG`), the values the protobuf
  carries. Transfer-list length, token-transfer-list length and the per-body field caps are not
  checked; a list long enough to matter is refused on the transaction's size instead.
  `eth_sendRawTransaction` is a different path and is not capped here.
- **Keeping every refusal.** The newest 1,000 are kept; `--max-rejections 0` keeps them all. They
  are cloned into every snapshot and written into every `--state` dump, so an uncapped list costs
  more than it looks: 20,000 refusals wrote a 9.8 MB state file, 543 KB capped.
- **`anvil_dumpState`, `anvil_loadState` and `anvil_reset` over JSON-RPC.** State does persist
  across restarts, through `--state` and `--dump-state` on the command line.
- **Base32 key aliases.** The mirror's `alias` field is null; hanvil mints EVM-address aliases,
  which `evm_address` already carries.
- **`eth_accounts` as the relay answers it.** The relay's is empty because it holds no keys and
  refuses `eth_sendTransaction` outright. hanvil implements that cheat, so `eth_accounts` names the
  thirty accounts it will send for, in id order, as Anvil does. Note the first ten are long-zero
  accounts: `eth_sendTransaction` works for them because the node holds the key, but the key
  printed for 0.0.1002 derives to a *different* EVM address, so a client signing locally wants the
  alias accounts, 0.0.1012 upward.
- **Record file hashes.** A block's `hash` is a 32-byte keccak over its own fields, and
  `hapi_version` is null because hanvil is not a consensus node and will not claim a version.
- **Itemised inner transfers.** A transaction's `transfers` list carries the fee and the top-level
  value transfer, not value moved by an inner call.
- **A live exchange rate.** Fixed at 1 ℏ = 12 ¢, and it never expires.
- **Key lists and threshold keys.** Accounts hold one key.
- **The mirror node's gRPC API on port 5600.** `Client.forLocalNode()` points its mirror network
  there, so `TopicMessageQuery` finds nothing listening and retries twenty times before giving up.
  Read topic messages over REST instead, or point the SDK's mirror network at a real one.
- **The relay's WebSocket endpoint on port 8546.** `eth_subscribe` has nothing to connect to. Watch
  logs the way `ethers` and `viem` do without a socket: `eth_newFilter`, then
  `eth_getFilterChanges`. `eth_newPendingTransactionFilter` is refused, because one block is mined
  per transaction and nothing is ever pending.
- **Forking testnet or mainnet state.** The node starts from its own genesis every time and opens
  no outbound socket. `--state` replays a file hanvil itself wrote.
- **`hanvil validate` and `validate-semantic` on `network: testnet`.** Both boot the in-process
  chain for the app, and on testnet there is nothing to boot; `hanvil run` is the testnet path.
  `mainnet` is refused by the recipe loader with upstream's own `Mainnet is not allowed.`
- **`assert`, `phases` and `advanceTimeSeconds` on `network: testnet`**, which are refused at load
  rather than skipped: they read the chain in this process, and testnet is not it.
  `snapshotPerAttempt` is false there for the same reason. `hedera-harness` has none of these on
  testnet either, so a recipe written for it is unaffected.
- **Public testnet itself, as a verified claim.** The testnet path is exercised end to end in
  `tests/run.rs` against a second hanvil standing in for the network — real protobuf, real ECDSA
  over `keccak256(bodyBytes)`, real gRPC, real receipts, a real `CryptoDelete` the account signs
  for itself. Node addressing, the real fee schedule, mirror lag and TLS have not been run against
  `0.testnet.hedera.com`. Point `chainValidation.node` at it and they will be. This is about
  `hanvil run`'s chain tier only: the x402 rail does settle on public testnet, and the transactions
  are on HashScan.
- **`hanvil run` with `agent: cursor`.** The preset and its `.cursor/mcp.json` delivery are ported
  line for line and have not been run against a Cursor install.
- **SMOKE's HTTP status on Chromium older than 109.** It is read from
  `PerformanceNavigationTiming.responseStatus`; where that is absent the status check is skipped
  and the render, console and forbidden-text checks still run.
- **A hermetic harness.** `hanvil run` vendors skills by cloning `hedera-dev/hedera-skills`
  (`--no-skills` turns it off), `hanvil init` clones `scaffold-hbar`, and the agent CLI, `npx` and
  the app's own commands make whatever calls they make. The node itself still opens no outbound
  socket.
- **Windows.** Children are killed by process group with `pkill -g`. The harness runs on macOS here
  and on ubuntu in CI.
- **A facilitator in Rust.** `hanvil toll` supervises a Node process, because the x402 `exact`
  scheme for Hedera is TypeScript (`@x402/hedera`) and a Rust one would be a second copy of its
  wire format. `hanvil toll` without Node fails saying so rather than starting half a rail.
- **x402 pricing in anything but HBAR.** `asset: "0.0.0"`, amounts in tinybar, so there is no token
  association step. `@x402/hedera` supports HTS tokens; the bundled service does not configure
  them. Schemes other than `exact`, and x402 v1, are not covered either.
- **A CAIP-2 id of hanvil's own.** `@x402/hedera` hardcodes
  `SUPPORTED_HEDERA_NETWORKS = ["hedera:mainnet", "hedera:testnet"]`, so the local rail is quoted
  as `hedera:testnet` and names the chain separately through `nodeUrl`. A receipt records both, or
  a local payment would claim testnet.
- **Hedera mainnet.** `HEDERA_NETWORK` takes `local` and `testnet` and refuses anything else by
  name. [Blocky402](https://blocky402.com) runs a mainnet facilitator; this rail has never been
  pointed at it, and nothing here has ever held value. The testnet leg *is* run —
  [the deployed service](#hanvil-toll--x402-on-the-local-chain) and its settlements are on
  HashScan.

### System contracts

Hedera puts three system contracts at fixed addresses in the EVM. hanvil emulates none of them:

| Entity | Address | What it is |
| --- | --- | --- |
| `0.0.359` | `0x…0167` | Hedera Token Service |
| `0.0.360` | `0x…0168` | exchange rate, `tinycentsToTinybars(uint256)` |
| `0.0.361` | `0x…0169` | pseudorandom seed, `getPseudorandomSeed()` (HIP-351) |

An address with no code is not an error in the EVM. A call to one succeeds and returns nothing, so
a contract calling `createFungibleToken` on an unemulated chain would read the call as having
worked and carry on with a token that does not exist. One asking `0x…0169` for a random seed would
take the zero it did not get as the seed.

So genesis etches bytecode at all three that reverts with `Error(string)`:

```
hanvil: HTS system contract not emulated; see README#system-contracts
hanvil: exchange rate system contract not emulated; see README#system-contracts
hanvil: PRNG system contract not emulated; see README#system-contracts
```

viem, ethers and `cast` all decode that, and `eth_call` returns it as
`{"code": 3, "data": "0x08c379a0…"}`. `anvil_setCode` overwrites it if you want a mock there.

The Hedera Account Service (HIP-632) is not stubbed. HIP-632 names its functions and not its
address, and hanvil does not etch an address it cannot cite.

Etched bytecode, not a precompile: what a caller sees is identical, and it survives
`evm_snapshot` / `evm_revert` because it is part of the chain state that gets cloned.

## `hanvil run` — the harness

`hanvil run` reads a `hedera-harness` schema v3 recipe, boots the network in-process, checks out a
`harness/run-<name>-<hex>` branch, and runs attempts until the recipe passes or `maxAttempts` is
spent. An attempt is five stages. A stage that fails skips the ones after it, and a failed
attempt's findings become the next attempt's repair prompt.

| Stage | What runs | Artifact |
| --- | --- | --- |
| 1 GENERATE | the agent CLI — `agent: claude` or `cursor`, or any `generator.command` — with the PRD as its prompt | `logs/generator-attempt-N.log` |
| 2 ASSERT | required and forbidden files, `validators/static.json`, the secret scan, `validators/commands.json` | `logs/validation-attempt-N.json` |
| 3 CHAIN | `chainValidation.deploy` commands, then `advanceTimeSeconds`, then `assert[]` on the in-process chain | `logs/chain-ledger-attempt-N.json` |
| 4 SMOKE | the app's dev server, then each route of `validators/playwright-smoke.yaml` in headless Chromium over `@playwright/mcp` | `logs/playwright-gate-attempt-N.json` |
| 5 EVALUATE | a validator agent with the same Playwright MCP server, judging `eval.json` in the browser | `logs/evaluation-attempt-N.json` |

```
hanvil run [SPEC] [--max-attempts N] [--new | --continue BRANCH] [--workspace DIR] [--no-skills]
hanvil doctor [SPEC] [--recipe-only]
hanvil validate [SPEC]              # ASSERT, CHAIN, then SMOKE when each is clean; no agent
hanvil validate-semantic [SPEC]     # EVALUATE only, against the workspace as it is
hanvil init [DIR] [--repo URL] [--ref REF] [--template NAME] [--skip-install]
```

Before GENERATE the chain is snapshotted — `Chain::snapshot()`, a clone under the lock, 5 µs.
After a failed attempt with budget left it is reverted, so the repair starts on the chain the
failed attempt started on, and the repair prompt says so. After every validation the chain is
written to `logs/chain-state-attempt-N.json`, and `hanvil --state <that file>` boots the network as
the attempt left it.

Every subprocess — agent, deploy commands, dev server, validator — receives `HANVIL_RPC_URL`,
`HANVIL_MIRROR_URL`, `HANVIL_GRPC_URL`, `HEDERA_NETWORK=local` and
`HARNESS_SIGNER_{ACCOUNT_ID,EVM_ADDRESS,PRIVATE_KEY}`. The signer's private key reads
`<redacted by hanvil>` in every prompt file. Ctrl-C or SIGTERM kills the agent, the dev server and
the browser by process group, writes `status.json` with `"phase": "interrupted"`, and exits 130.

The [recipe schema](docs/reference.md#recipe-schema), [a full run stage by
stage](docs/reference.md#a-full-run-stage-by-stage) and the [six deviations from the
TypeScript](docs/reference.md#six-deviations-from-the-typescript-harness) are in the reference.

### The chain ledger

This is the part a mirror node cannot give you.

A transaction refused before consensus — `INVALID_SIGNATURE`, a `value` that is not a whole number
of tinybar, a payer that cannot cover the fee — leaves no record, no receipt and no mirror row.
That is how Hedera works. `nodeTransactionPrecheckCode` comes back in `TransactionResponse` and
nothing is written anywhere. The error returned to the caller is the only trace, so an app that
catches it and carries on destroys the evidence. A harness reading a mirror node then sees an
effect that is missing, with no cause. `hedera-harness` reads a mirror node.

hanvil is the node, so it keeps them. From `tests/harness/.harness/spec-ledger.yaml`, where the app
sends three transfers and two of them ask to move one weibar:

```
[hanvil] Chain ledger — attempt 1 — 3 transaction(s), 2 rejected before consensus
  #  kind                 payer     result                                                            entity  at
  1  ETHEREUMTRANSACTION  0.0.1002  SUCCESS                                                           —       +0ms
  2  ETHEREUMTRANSACTION  0.0.1002  REJECTED Invalid params: 1 weibar is not a multiple of 10^10 (1…  —       +6ms
  3  ETHEREUMTRANSACTION  0.0.1002  REJECTED Invalid params: 1 weibar is not a multiple of 10^10 (1…  —       +12ms
[hanvil] Chain assertions — 0 of 1 passed
```

The assertion wanted three transactions and counted one. Without the ledger, the finding is that
count and nothing else. With it, the finding carries the cause, and so does the repair prompt:

```json
{
  "id": "chain:0:transactions",
  "message": "Chain assertion 0 (transactions) failed: 1 successful ETHEREUMTRANSACTION transaction(s) since the attempt began, fewer than 3",
  "details": "2 ETHEREUMTRANSACTION submission(s) were refused before consensus (Invalid params: 1 weibar is not a multiple of 10^10 (1 tinybar); the relay rejects such values ×2, payer 0.0.1002) — no record exists for them on any Hedera network"
}
```

Outside a run the same list is one call away, for anyone driving hanvil with viem, hardhat or
foundry and wondering where a transaction went:

```
curl -s -X POST localhost:7546 -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"hanvil_rejections","params":[]}' | jq .result
```
```json
[
  {
    "at": "1789107752.728516000",
    "kind": "ETHEREUMTRANSACTION",
    "payer": "0.0.1002",
    "from": "0x00000000000000000000000000000000000003ea",
    "code": null,
    "reason": "Invalid params: 1 weibar is not a multiple of 10^10 (1 tinybar); the relay rejects such values"
  }
]
```

`code` is the `ResponseCodeEnum` number for a HAPI refusal and `null` for a JSON-RPC one, which has
none. An optional first argument caps the rows and keeps the most recent.

The validator agent gets the same table before it opens the browser, so a UI that toasts success
over a refused transaction is an issue rather than a pass. Upstream's validator prompt calls the
mirror node keyless ground truth, and on a refusal the mirror node has nothing to say.

### Against the TypeScript harness

`hanvil run` covers `hedera-harness`'s surface: the same five commands, the same five stages, the
same recipe schema, the same artifacts and console strings — and `network: testnet`, so there is no
second tool to install. On testnet it does what upstream does and no more. On `network: local`,
where the chain is in this process, it adds:

- A snapshot before every attempt and a revert after a failed one.
- `assert[]` in the recipe, evaluated on the chain struct. The TypeScript has no mirror client; it
  hands the validator agent a mirror URL.
- The chain ledger, including refused transactions. A mirror node has no row for one, so this half
  of the chain is not reachable from the TypeScript harness at all — not as a missing feature, but
  because the data is not written on any Hedera network.
- Assertions it cannot express: `rejections`, `contract: created` with an `event` counting decoded
  logs, and `phases` moving the chain clock between assertion sets.
- `report.json` carrying `chainLedger`, so CI asserts on what a run did to the chain without
  parsing rows.
- A repair that sends the same refused transaction as the attempt before it is told so.
- Replay of any attempt's chain with `--state`.
- No operator id, no key, no environment variable. `chainValidation` is two lines.
- No `npm install` for the harness itself.

## The TypeScript harness on hanvil

Upstream `hedera-harness` also runs against hanvil, over the network, with no credentials.
Measured 2026-09-08, five runs, 3.6-4.3 s wall clock:

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

Nothing is set in the environment — no `HEDERA_OPERATOR_ID`, no `HEDERA_OPERATOR_KEY`.
`.github/workflows/ci.yml` runs this on every push with no secrets, and asserts the signer reached
`network: local` rather than trusting the verdict.

`network: "local"` is not in `hedera-harness` yet. It is two open PRs against
`hedera-dev/hedera-harness` `dev`:

| PR | What it does |
| --- | --- |
| [#47](https://github.com/hedera-dev/hedera-harness/pull/47) | `network: "local"` for the signer, `doctor` and the validator prompt |
| [#48](https://github.com/hedera-dev/hedera-harness/pull/48) | `evm_snapshot` before an attempt, `evm_revert` after a failed one |

Without the second, a repair attempt inherits whatever the previous attempt wrote on chain. On a
recipe that mines three blocks per attempt and then fails, attempts end at block `0x3`, `0x6`,
`0x9`; with it, `0x3`, `0x3`, `0x3`. #48 stacks on #47 and contains its commits.

## `hanvil toll` — x402 on the local chain

`hanvil toll` serves an [x402](https://x402.org) payment rail on the chain in this process: a
facilitator, a metered service, and three predefined accounts wired up as payer, destination and
fee payer.

```
$ hanvil toll
x402 facilitator  http://127.0.0.1:4020
Service           http://127.0.0.1:4021
Settles on        the in-process chain (127.0.0.1:50211)
Price             100000 tinybar per call

feePayer          0.0.1004  the facilitator submits and pays the fee
payer             0.0.1002  the account a call is charged to
payTo             0.0.1003  where a settled toll lands
```

An x402 payment on Hedera is a `CryptoTransfer` the client partially signs and the facilitator
co-signs and submits as fee payer. One paid call settles in 0.081 s, read off hanvil's mirror:

```
0.0.1004-1789194758-592048710 CRYPTOTRANSFER SUCCESS
  0.0.98    +10000   the fee
  0.0.1002 -100000   the payer
  0.0.1003 +100000   payTo
  0.0.1004  -10000   the facilitator, which paid the fee
```

Every way that payment can fail is refused before consensus. Replay a payment header, and the two
sides disagree about how much is knowable:

```
$ yarn replay
[replay] first request  200  settled 0.0.1004@1789194847.284997026
[replay] replayed once  402  refused transaction_failed      <- all x402 can tell you

[replay] hanvil_rejections grew by 1:
{ kind: 'CRYPTOTRANSFER', payer: '0.0.1004', code: 11, reason: 'DUPLICATE_TRANSACTION' }
```

`transaction_failed` is what a developer on testnet gets, and there is nothing on any mirror node
to look up. After the paid requests above, hanvil's mirror held three `CRYPTOTRANSFER SUCCESS` rows
and nothing else. The duplicate existed only in `hanvil_rejections`.

The facilitator is [x402's own reference implementation](https://github.com/x402-foundation/x402),
not a reimplementation. The single change is `buildHederaClient`, the extension point its own
example uses:

```ts
toFacilitatorHederaSigner({
  getAddresses: () => [accountId],
  signAndSubmitTransaction: createHederaSignAndSubmitTransaction(
    () => Client.forNetwork({ [HANVIL_GRPC_URL]: AccountId.fromString("0.0.3") })
             .setOperator(accountId, key),
    key),
  verifyPayerSignature: createHederaVerifyPayerSignature({ mirrorNodeUrl: HANVIL_MIRROR_URL }),
  preflightTransfer:    createHederaPreflightTransfer({ mirrorNodeUrl: HANVIL_MIRROR_URL }),
})
```

Both `mirrorNodeUrl` overrides are load-bearing. `@x402/hedera` otherwise derives a mirror from the
CAIP-2 id and asks the public testnet mirror for the payer's key, which for a local account it does
not have. The wrong key refuses every partial signature while the error blames the signature.

One switch picks the rail, and the same service runs on both:

| `FACILITATOR_URL` | Settles on | Used by |
| --- | --- | --- |
| `http://127.0.0.1:4020` | hanvil, in-process | `hanvil toll`, `hanvil run`, local development |
| `https://api.testnet.blocky402.com` | Hedera testnet | a deployed service |

That second row is deployed: **https://hanvil-toll-production.up.railway.app**, the same directory
with `HEDERA_NETWORK=testnet`. One paid call from outside settles in 4.91 s through Blocky402 —
[`0.0.7162784@1789204637.167233288`](https://hashscan.io/testnet/transaction/0.0.7162784@1789204637.167233288),
payer `0.0.10497245` −100,000 tinybar, payTo `0.0.10497252` +100,000, and the 268,330-tinybar fee
paid by the facilitator, which is the whole point of the scheme. Every settled call writes an
`x402.receipt.v1` message to [topic `0.0.10497255`](https://hashscan.io/testnet/topic/0.0.10497255),
so the payment history is provable from the chain rather than from the service's logs. The same
`yarn pay`, one environment variable apart, is 0.081 s against hanvil — sixty times faster, and
free.

`examples/toll/.harness/` is a recipe that rebuilds the metered service from `hedera-harness`'s own
x402 PRD, with chain assertions the app cannot fake: a topic created and written to, at least one
`CRYPTOTRANSFER` settled, and `rejections: { atMost: 0 }` — an assertion that cannot be written
against testnet at all, because there would be nothing to read. Details in
[`examples/toll/README.md`](examples/toll/README.md).

## How it is built

One `Chain` struct behind one `RwLock`. Every listener takes the same lock, so a transfer over
JSON-RPC is visible to the mirror in the same millisecond. `evm_snapshot` clones the struct;
`evm_revert` swaps it back.

```
 JSON-RPC :7546 ─┐
 mirror   :5551 ─┼─→ RwLock<Chain> ─→ accounts · blocks · receipts · logs · topics ·
 HAPI     :50211 ┤                    HAPI records · revm CacheDB
 hanvil run ─────┤                    (balances authoritative in tinybar)
 hanvil toll ────┘
   │ spawns: agent CLI · git · deploy commands · dev server · @playwright/mcp · x402 facilitator
```

`hanvil run` and `hanvil toll` hold the same lock. The signer, the snapshot, the revert, the
assertions and the state dump are method calls on `Chain` under `write()` or `read()`, never across
an `.await`. The agent, `git`, the deploy commands, the dev server, the browser and the facilitator
are subprocesses in their own process groups, and the harness reads their pipes.

`revm` executes, `alloy` decodes and recovers senders, `axum` serves all three listeners, `tonic`
and `prost` speak HAPI over the 130 vendored protobuf files, `clap` reads the flags,
`serde_yaml_ng` reads the recipe, `regex` runs the secret scan.

The node makes no outbound network calls. It never fetches anything. The processes `hanvil run` and
`hanvil toll` spawn are listed above, and they make whatever calls they make.

The build is laid out in [`docs/code-plan.md`](docs/code-plan.md). Every claim about how Hedera's
own tooling behaves is pinned to a file and line in [`docs/research.md`](docs/research.md).

## Authorship and AI use

hanvil is a solo project, built from a first commit on 2026-09-07 by Vicky Prasad
([github.com/vickyshaw29](https://github.com/vickyshaw29)), who set both the direction and the bar
it had to clear.

The decisions were the work. Build the node first and make the existing harness run on it, rather
than begin with a rewrite. Copy `hiero-local-node`'s ports, accounts and keys byte for byte, so
nothing downstream has to know it changed. Run the EVM in tinybar rather than wei, and pay for that
in every conversion, because Hedera does. Send two narrow PRs to `hedera-dev/hedera-harness` rather
than fork it. Port the harness into the same binary only once the node was finished, so `hanvil run`
is a second surface on one chain and not a second product. Keep the transactions Hedera refuses
before consensus — the one thing a node can do that no mirror node can. Declare every hole rather
than let a judge find a stub.

Those decisions are written down ahead of the code, and the gates that enforce them are in the
repository:

| Path | What it is |
| --- | --- |
| [`.claude/CLAUDE.md`](.claude/CLAUDE.md) | the engineering standard every line was held to: one `Chain` behind one lock, every id a newtype, error codes mapped 1:1 to upstream, the deny list, the definition of done, and the anti-patterns that get reverted on sight |
| [`docs/code-plan.md`](docs/code-plan.md) | the build plan the code followed |
| [`docs/research.md`](docs/research.md) | every fact this project relies on, with the upstream file and line that settles it. A claim that is not in here does not go in the README |
| [`.claude/hooks/`](.claude/hooks) | `pre-commit-gate.sh` fails a commit that adds an `unwrap` under `src/`, touches a forbidden path, or does not pass `cargo fmt` and `clippy -D warnings` |
| [`.claude/skills/`](.claude/skills) | the repeatable procedures. `/gate` runs every CI gate locally; `/bench` produces every number this README is allowed to print; `/fact` settles a claim from source or answers "not found" |
| [`.claude/agents/`](.claude/agents) | the review agents. `spec-checker` diffs a type field by field against `openapi.yml` or a `.proto`; `standard-reviewer` reviews a diff against the standard above; `upstream-reader` answers "how does the SDK actually do this" with evidence |

The code was written with Claude Code against that standard and through those gates. Every commit
carries a `Co-Authored-By: Claude` trailer, so `git log` shows exactly where that applies rather
than leaving it to be taken on trust.

No number in this README was estimated. Every figure in [Measured](#measured) was produced by a
command — printed beside it, or listed under [reproducing the
numbers](docs/reference.md#reproducing-the-numbers) — on the machine and date named there. Where a
number could not be obtained the README says so instead: Solo never booted here, so no Solo boot
time is quoted. Every response shape is copied from a spec, with the path and line in a comment next
to the struct. The 31 holes in [what is not emulated](#what-is-emulated-and-what-is-not), on top of
the [five headline limits](#why-you-might-not), are declared because a stub a judge finds costs more
than ten that are named.

Not to be confused with the above: [`src/harness/prompts/`](src/harness/prompts) is product code —
the prompts `hanvil run` sends to the agent it drives, ported from `hedera-harness`. They had no
part in building hanvil.

## Licence

MIT. Vendored HAPI protobufs are Apache-2.0. The harness's prompt templates, recipe schema,
skeletons and console strings derive from `hedera-dev/hedera-harness`, MIT — see
[NOTICE](NOTICE).
