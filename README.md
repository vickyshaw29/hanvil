# hanvil

A local Hedera network and a coding-agent harness in one binary. `hanvil` answers its first
JSON-RPC call 5 ms after exec, where `hiero-local-node` takes 50.6 s and 3.8 GB across 18
containers. It prints thirty pre-funded accounts and serves the three protocols a Hedera app
already speaks — JSON-RPC on 7546, mirror node REST on 5551, HAPI gRPC on 50211 — from one
in-memory chain, on the ports `hiero-local-node` uses, so `@hiero-ledger/sdk`, viem, hardhat and
foundry connect to it unchanged. Unlike the Docker stack it can snapshot the whole chain and put
it back, and survive a restart. Both numbers, and what the stack gives that Hanvil does not, are
under [Measured](#measured).

`hanvil run` drives a coding agent through a `hedera-harness` recipe — generate, assert, chain,
smoke, evaluate — with the network in the same process. That is not a faster version of the
TypeScript harness; it is a different amount of truth. Every attempt starts on a chain snapshot
and a failed one is reverted, the recipe asserts on accounts, contracts and topics with no mirror
round trip, any attempt's chain boots again from its state dump, and the run reports every
transaction the app sent — including the ones the node refused, which leave no record, no receipt
and no mirror row anywhere on Hedera. A harness that reads a mirror node cannot see those. No
testnet account, no HBAR, no credentials.

## Measured

On an M-series Mac, 10 CPUs, Docker given 8 GB, 2026-09-11, release build. hanvil: median of
five runs, eleven for the banner. `hiero-local-node` (`research/hiero-local-node`, v2.40.2):
median of three, images pulled first and the pull not counted, stack fully stopped between runs.
Each was measured on an otherwise idle machine.

| | hanvil | hiero-local-node | how it was measured |
| --- | --- | --- | --- |
| Boot to a JSON-RPC answer | 5 ms | 50.6 s | first successful `eth_chainId` on :7546, timed from exec |
| Boot, as each reports it | 1 ms | 45.9 s | `Started in 1 ms`; `Hedera Local Node successfully started!` |
| Resident memory | 5.4 MB | 3,898 MB | `ps -o rss=`; `docker stats --no-stream` summed over the stack |
| On disk | 9.2 MB | 9.4 GB | `ls -l target/release/hanvil`; `docker images` over `docker compose config --images` |
| Processes | 1 | 18 running, 23 created | `docker ps`, `docker ps -a` |
| Snapshot and restore the chain | `evm_snapshot` / `evm_revert` | not supported | — |
| Chain survives a restart | `--state FILE` | not supported | `hiero-local-node` README:630-631 |

Ten thousand times the boot, seven hundred times the memory, a thousand times the disk. The last
two rows are `hiero-local-node`'s own answer to its own FAQ: *"Can I stop the local node, save
its state then start it again after a while? No, currently the local node doesn't support network
freezing. Once you stop it, the next start will be with a genesis state and all of your
accounts/contracts/tokens will be wiped."*

What that buys is not free: `hiero-local-node` runs the real consensus node, the real mirror node
and the real relay, and Hanvil emulates them — see [what is emulated, and what is
not](#what-is-emulated-and-what-is-not). Nothing here says the stack is badly built. It says an
inner loop should not cost 50 seconds and 3.8 GB.

Hanvil's own numbers, same machine and day:

| | hanvil | how it was measured |
| --- | --- | --- |
| Accounts pre-funded | 30, 10,000 ℏ each | the boot banner |
| `hanvil doctor`, every check | 60 ms | `time hanvil doctor` in a copy of `tests/harness` |
| `hanvil run`, one attempt, the fixture's fake agent | 0.23 s | `time hanvil run --no-skills` in the same copy: node boot, signer, snapshot, agent, ASSERT, CHAIN, state dump, checkpoint commit, sweep |
| Signer provisioned on the chain | 152 µs | `chain_signer_provisioned.durationMicros` in `.harness/runs/harness.log.jsonl` |
| Chain snapshot before an attempt | 10 µs | `chain_snapshot_taken.durationMicros`, same log |
| Chain state dump for replay | 35,122 bytes in 133 µs | `chain_state_written`, same log |
| Full run with `claude` on `examples/hcs-receipts-api` | 9 min 32 s, 2 attempts | `report.json.durationMs`, measured 2026-09-10; the breakdown is under [Harness](#harness) |

CI asserts the median boot stays under 100 ms on ubuntu and macos runners
(`.github/workflows/ci.yml`).

To reproduce the comparison:

```
git clone https://github.com/hiero-ledger/hiero-local-node && cd hiero-local-node
npm install && npm run build && docker compose pull      # the pull is not timed
node ./build/index.js stop
time node ./build/index.js start                         # to its own banner
docker stats --no-stream --format '{{.MemUsage}}'
```

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

`--mirror-port` moves the REST listener, but `@hiero-ledger/sdk` hardcodes `5551` for a
`127.0.0.1` mirror (`hiero-sdk-js/src/MirrorNode.js:59-60`), so its REST-backed queries follow the
default and not the flag. `--state FILE` reads the chain at boot if the file is there and writes it back on exit, so a
restart continues where the last run stopped; the file decides the chain id and the accounts, and
the genesis flags are ignored. `--dump-state FILE` writes without reading. `--block-time SECONDS`
mines an empty block on that interval — transactions still mine their own block the moment they
arrive, so this makes time move, it does not batch.

## Endpoints

| Port | What | Surface |
| --- | --- | --- |
| 7546 | JSON-RPC, relay shape | `eth_chainId` `eth_blockNumber` `eth_getBalance` `eth_getCode` `eth_getStorageAt` `eth_getTransactionCount` `eth_gasPrice` `eth_maxPriorityFeePerGas` `eth_feeHistory` `eth_call` `eth_estimateGas` `eth_sendRawTransaction` `eth_sendTransaction` `eth_getTransactionByHash` `eth_getTransactionReceipt` `eth_getBlockBy{Number,Hash}` `eth_getBlockReceipts` `eth_getLogs` `eth_newFilter` `eth_newBlockFilter` `eth_getFilterChanges` `eth_getFilterLogs` `eth_uninstallFilter` `eth_getBlockTransactionCountBy{Hash,Number}` `eth_getTransactionByBlock{Hash,Number}AndIndex` `net_version` `net_listening` `web3_clientVersion` `web3_sha3` |
| 7546 | Anvil cheats | `evm_snapshot` `evm_revert` `evm_mine` `evm_increaseTime` `evm_setNextBlockTimestamp` `anvil_setBalance` `anvil_setCode` `anvil_setNonce` `anvil_setStorageAt` `anvil_impersonateAccount` `anvil_stopImpersonatingAccount` `anvil_mine` `anvil_nodeInfo`, and the `hardhat_` aliases. Plus `hanvil_rejections`, which is Hanvil's own |
| 5551 | Mirror node REST | `/api/v1/accounts/{id\|alias\|evm}` `/accounts/{id}/tokens` `/balances` `/transactions` (`account.id` `transactiontype` `result` `timestamp` `limit` `order`) `/transactions/{0.0.x-sss-nnn}` `/contracts/{id\|address}` `/contracts/{id}/results` `/contracts/results/{hash\|txId}` `/contracts/results/logs` `/topics/{id}` `/topics/{id}/messages` `/topics/{id}/messages/{n}` `/blocks` `/blocks/{number\|hash}` `/network/nodes` `/network/fees` `/network/exchangerate` |
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

A transaction that fails precheck leaves no record, no receipt and no mirror row, as on Hedera.
Hanvil additionally keeps it in a list of its own, which `hanvil run` reads for [the chain
ledger](#the-chain-ledger) and `hanvil_rejections` answers over JSON-RPC. The HAPI and mirror
surfaces are unchanged — a client sees exactly what a node would show it — and `hanvil_` is a
cheat namespace beside `anvil_`, not a Hedera endpoint. `--state` carries the list; a state file
written before it existed still loads.

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
- The relay's WebSocket endpoint on port 8546. `eth_subscribe` has nothing to connect to. Watch
  logs the way `ethers` and `viem` do without a socket: `eth_newFilter` then `eth_getFilterChanges`,
  which Hanvil serves. `eth_newPendingTransactionFilter` is refused — one block is mined per
  transaction, so nothing is ever pending.
- Forking testnet or mainnet state. Hanvil starts from its own genesis every time and makes no
  outbound calls; `--state` replays a file Hanvil itself wrote.
- `hanvil run` with `network: testnet`. Preflight and `doctor` refuse it with `Use network:
  local, or run this recipe with hedera-harness.`; the two PRs below are the testnet path.
  `mainnet` is refused by the recipe loader with upstream's own `Mainnet is not allowed.`
- `hanvil run` with `agent: cursor`. The preset and its `.cursor/mcp.json` delivery are ported
  line for line and have not been run against a Cursor install.
- SMOKE's HTTP status on a Chromium older than 109. It is read from
  `PerformanceNavigationTiming.responseStatus`; where that is absent the status check is skipped
  and the render, console and forbidden-text checks still run.
- A hermetic harness. `hanvil run` vendors skills by cloning `hedera-dev/hedera-skills`
  (`--no-skills` turns it off), `hanvil init` clones `scaffold-hbar`, and the agent CLI, `npx`
  and the app's own commands make whatever calls they make. The node itself still opens no
  outbound socket.
- Windows. Children are killed by process group with `pkill -g`; the harness runs on macOS here
  and on ubuntu in CI.

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

## Harness

`hanvil run` reads a `hedera-harness` schema v3 recipe (`.harness/spec.yaml` by default), boots
the network in-process, checks out a `harness/run-<name>-<hex>` branch, and runs attempts until
the recipe passes or `maxAttempts` is spent. An attempt is five stages; a stage that fails skips
the ones after it, and a failed attempt's findings become the next attempt's repair prompt.

| Stage | What runs | Result |
| --- | --- | --- |
| 1 GENERATE | the agent CLI — `agent: claude` or `cursor`, or any `generator.command` — with the PRD as its prompt | `logs/generator-attempt-N.log` and `.activity.log` |
| 2 ASSERT | required and forbidden files, `validators/static.json`, the secret scan, `validators/commands.json` | `logs/validation-attempt-N.json` |
| 3 CHAIN | `chainValidation.deploy` commands, then `advanceTimeSeconds`, then `assert[]` on the in-process chain | findings `chain:<i>:<kind>` in the same file, and `logs/chain-ledger-attempt-N.json` |
| 4 SMOKE | the app's dev server, then each route of `validators/playwright-smoke.yaml` in headless Chromium over `@playwright/mcp` | `logs/playwright-gate-attempt-N.json` |
| 5 EVALUATE | a validator agent with the same Playwright MCP server, judging `eval.json` in the browser | `logs/evaluation-attempt-N.json` |

Before GENERATE the chain is snapshotted — `Chain::snapshot()`, a clone under the lock. After a
failed attempt with budget left it is reverted, so the repair starts on the chain the failed
attempt started on, and the repair prompt says so. After every validation the chain is written to
`logs/chain-state-attempt-N.json`: `hanvil --state <that file>` boots the network as the attempt
left it, and `hanvil run --continue <branch>` reloads the last one. Every subprocess — agent,
deploy commands, dev server, validator — receives `HANVIL_RPC_URL`, `HANVIL_MIRROR_URL`,
`HANVIL_GRPC_URL`, `HEDERA_NETWORK=local` and `HARNESS_SIGNER_{ACCOUNT_ID,EVM_ADDRESS,PRIVATE_KEY}`,
and the generator prompt carries a `## Local Hedera network` section saying the same.

### The chain ledger

CHAIN prints every transaction the attempt sent, in consensus order, and writes the same rows to
`logs/chain-ledger-attempt-N.json`. The rows that matter are the ones a mirror node does not have.

A transaction refused before consensus — `INVALID_SIGNATURE`, a `value` that is not a whole number
of tinybar, a payer that cannot cover the fee — leaves no record, no receipt and no mirror row.
That is how Hedera works: `nodeTransactionPrecheckCode` comes back in `TransactionResponse` and
nothing is written anywhere. The error returned to the caller is the only trace, so an app that
catches it and carries on destroys the evidence, and a harness that reads a mirror node sees an
effect that is missing with no cause. `hedera-harness` reads a mirror node.

Hanvil is the node, so it keeps them. From `tests/harness/.harness/spec-ledger.yaml`, where the
app sends three transfers and two of them ask to move one weibar:

```
[hanvil] Stage 3/5 CHAIN
[hanvil] Chain deploy — seed — bash .harness/seed-ledger.sh
[hanvil] Chain ledger — attempt 1 — 3 transaction(s), 2 rejected before consensus
  #  kind                 payer     result                                                            entity  at
  1  ETHEREUMTRANSACTION  0.0.1002  SUCCESS                                                           —       +0ms
  2  ETHEREUMTRANSACTION  0.0.1002  REJECTED Invalid params: 1 weibar is not a multiple of 10^10 (1…  —       +6ms
  3  ETHEREUMTRANSACTION  0.0.1002  REJECTED Invalid params: 1 weibar is not a multiple of 10^10 (1…  —       +12ms
[hanvil] Chain assertions — 0 of 1 passed
```

The assertion wanted three transactions and counted one. Without the ledger the finding is that
count and nothing else. With it the finding carries the cause, and so does the repair prompt:

```json
{
  "id": "chain:0:transactions",
  "category": "chain",
  "message": "Chain assertion 0 (transactions) failed: 1 successful ETHEREUMTRANSACTION transaction(s) since the attempt began, fewer than 3",
  "details": "2 ETHEREUMTRANSACTION submission(s) were refused before consensus (Invalid params: 1 weibar is not a multiple of 10^10 (1 tinybar); the relay rejects such values ×2, payer 0.0.1002) — no record exists for them on any Hedera network"
}
```

Outside a run, the same list is one call away — for anyone driving `hanvil` with viem, hardhat
or foundry and wondering where a transaction went:

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

An optional first argument caps the rows and keeps the most recent. `code` is the
`ResponseCodeEnum` number for a HAPI refusal and `null` for a JSON-RPC one, which has none.

The repair prompt gains a `## Chain Ledger` section with the whole table and one sentence saying
a `REJECTED` row cannot be looked up anywhere. The validator agent gets the same table before it
opens the browser, so a UI that toasts success over a refused transaction is an issue rather than
a pass — upstream's validator prompt calls the mirror node keyless ground truth, and on a refusal
the mirror node has nothing to say. The table clips a relay message at 56 characters; the JSON
artifact and the finding keep the sentence. The ledger is rebuilt after SMOKE, because the app
goes on working the chain while the browser gate drives it.

To reproduce, with the binary built:

```
cp -R tests/harness /tmp/ledger && cd /tmp/ledger
git init -q -b main && git add -A && git commit -qm fixture
hanvil run .harness/spec-ledger.yaml --max-attempts 1 --no-skills
cat .harness/runs/*/logs/chain-ledger-attempt-1.json
```

```
cp -R examples/hcs-receipts-api /tmp/receipts && cd /tmp/receipts
git init -q -b main && git add -A && git commit -qm seed
hanvil doctor          # ✔ on every line under `env -i PATH="$PATH" HOME="$HOME"`
hanvil run             # needs `claude` on PATH, Node 20+ and npx
```

The recipe is schema v3 as `hedera-harness` reads it — the same keys, defaults, error strings,
prompts and artifact layout, ported from `dev` @ `587a2f3` and cited by file and line in
`src/harness/` — with five additions under `chainValidation`, all optional. Upstream's loader
ignores unknown keys under `chainValidation`, so a recipe using them still loads on
`hedera-harness`:

```yaml
chainValidation:
  enabled: true
  network: local            # no operator block; the harness funds the signer from 0.0.1002
  fundingHbar: 50
  deploy:
    commands:
      - { name: seed, command: node scripts/seed.js, timeoutMs: 60000 }
  snapshotPerAttempt: true  # false keeps chain state across attempts, as the TypeScript does
  advanceTimeSeconds: 0     # evm_increaseTime before the assertions
  assert:                   # each one that fails is a finding, id chain:<i>:<kind>
    - { topic: created, messagesAtLeast: 3 }        # or topic: 0.0.N
    - { account: signer, minBalanceHbar: 40 }       # or 0.0.N / 0x…; exists: / deleted:
    - { transactions: { type: CONSENSUSSUBMITMESSAGE, payer: signer, atLeast: 3 } }
    - { contract: created, event: "Stored(address,uint256)", atLeast: 3 }   # or contract: 0x…
    - { contract: created, deployed: true }
    - { rejections: { atMost: 0 } }                 # or type: / payer: to scope it
  phases:                   # each one advances the clock, runs its commands, then asserts
    - name: after-a-week
      advanceTimeSeconds: 604800
      deploy:
        commands: [{ name: claim, command: node scripts/claim.js }]
      assert:
        - { contract: created, event: "Expired(uint256)", atLeast: 1 }
```

`contract: created` is the newest contract on the chain, the meaning `topic: created` already
has, so a recipe can assert on a deployment whose address it never sees. `rejections` fails on
transactions the node refused — the rows a mirror node does not have, so `hedera-harness` cannot
express this assertion at all. A phase moves the clock *before* its commands, because
`increase_time` shifts the offset and `block.timestamp` only follows on the next mined block; the
flat `deploy` keeps its existing deploy-then-advance order. Assertion indices run on across the
flat block and every phase, so adding a phase never renumbers a finding id.

From `tests/harness/.harness/spec-phases.yaml`, where a contract reverts with
`Deadline: too early` until its window closes:

```
[hanvil] Chain assertions — 1 of 1 passed
[hanvil] Chain phase — after-a-week
[hanvil] Chain time advanced — 604800 s
[hanvil] Chain deploy — sweep — bash .harness/sweep-deadline.sh
[hanvil] Chain ledger — attempt 1 — 2 transaction(s)
  #  kind                 payer     result   entity                                               at
  1  ETHEREUMTRANSACTION  0.0.1002  SUCCESS  0x4388985fc3EFb7978b71b7fc59114aa64A42E285 (deploy)  +0ms
  2  ETHEREUMTRANSACTION  0.0.1002  SUCCESS  —                                                    +604800.0s
[hanvil] Chain assertions — 2 of 2 passed — phase after-a-week
Run PASSED
```

A week, in the time the sweep took. With `advanceTimeSeconds: 0` the same run fails and the
ledger reads `reverted: Deadline: too early`, which is what makes the pass evidence rather than
an assertion; both are in `tests/run.rs`.

The first run on the example, 2026-09-10, `agent: claude`, `--max-attempts 3`, as printed:

```
[hanvil] Chain signer provisioned — 0.0.1032 (0xb2e10a30e626e1e3dbf016fff7ab7528c43a4439)
[hanvil] Stage 1/5 GENERATE — attempt 1 [opus]
[hanvil] Chain snapshot taken — attempt 1 — 0x0
[hanvil] Stage 2/5 ASSERT
[hanvil] Stage 3/5 CHAIN
[hanvil] Chain deploy — seed — node scripts/seed.js
[hanvil] Chain assertions — 3 of 3 passed
[hanvil] Stage 4/5 SMOKE — booting dev server
[hanvil] Stage 5/5 EVALUATE — skipped — smoke gate failed
[hanvil] Attempt 1 FAILED — 1 open, 1 new
[hanvil] Chain state written — attempt 1
[hanvil] Workspace committed — harness: run attempt 1 failed @ 40d2bf3e
[hanvil] Chain reverted — attempt 2 starts on the state attempt 1 started on
[hanvil] Stage 1/5 GENERATE — repair, attempt 2 [opus, escalated — last attempt fixed nothing]
[hanvil] Chain snapshot taken — attempt 2 — 0x1
[hanvil] Stage 2/5 ASSERT
[hanvil] Stage 3/5 CHAIN
[hanvil] Chain assertions — 3 of 3 passed
[hanvil] Stage 4/5 SMOKE — booting dev server
[hanvil] Stage 5/5 EVALUATE — http://127.0.0.1:3000
[hanvil] Attempt 2 PASSED — All three checklist assertions verified in-browser via Playwright MCP …
[hanvil] Run finished: PASSED — 0 open, 1 fixed
[hanvil] Chain signer swept — 0.0.1032
Run PASSED
```

The one finding was `playwright:route:home:console`: the page had no favicon, Chromium logged the
404 as a console error, and the SMOKE gate counts that as upstream does. The repair added the
route. Where the 9 min 32 s went, from the `harness.log.jsonl` timestamps and `report.json`:

| | attempt 1 | attempt 2 |
| --- | --- | --- |
| GENERATE (`claude`, `opus`) | 354.1 s | 121.8 s |
| ASSERT (`npm install`, `node --check`) | 3.8 s | 0.06 s — install skipped, lockfile fingerprint unchanged |
| CHAIN (`node scripts/seed.js`, three assertions) | 0.4 s | 0.3 s |
| SMOKE (two routes, headless Chromium) | 6.0 s | 6.2 s |
| EVALUATE (validator agent over Playwright MCP) | skipped | 75.5 s |
| state dump, checkpoint commit, revert | 73 ms | 74 ms, no revert |

The attempt-2 dump is 43,643 bytes; `hanvil --state <it> --port 0` prints `Started in 0 ms` and
its mirror answers `GET /api/v1/topics/0.0.1033/messages` with the six messages the app wrote.

Against the TypeScript harness, on the same recipe:

- A snapshot before every attempt and a revert after a failed one. On testnet there is nothing
  to revert; PR #48 below does it over JSON-RPC against a local node; here it is a clone under
  the lock, 5 µs.
- `assert[]` in the recipe, evaluated on the chain struct. The TypeScript has no mirror client;
  it hands the validator agent a mirror URL.
- The chain ledger, including the transactions the node refused. A mirror node has no row for
  one, so this half of the chain is not reachable from the TypeScript harness at all — not as a
  missing feature, but because the data is not written on any Hedera network.
- Assertions it cannot express: `rejections` fails a run on those refusals; `contract: created`
  with an `event` counts decoded logs; `phases` moves the chain clock between assertion sets, so
  a one-week deadline is tested in the time a transaction takes rather than in a week.
- `report.json` carries `chainLedger` — transactions, succeeded, failed, rejected — so CI asserts
  on what a run did to the chain without parsing the rows.
- A repair that sends the same refused transaction as the attempt before it is told so, in the
  console and in its own prompt.
- Replay of any attempt's chain with `--state`.
- No operator id, no key, no environment variable. `chainValidation` is two lines.
- An activity log for `claude` as well as `cursor`: the `TOOL START edit /…/lib/hedera.js`
  lines are parsed out of `stream-json`.
- No `npm install` for the harness. Node is needed by the app under test and by
  `@playwright/mcp`, not by `hanvil`.

Five deviations from the TypeScript, each recorded in `docs/code-plan.md` §16: the `claude`
preset's idle timeout is 600 s, not 90 s (a `Bash` tool call is silent until it returns);
`CLAUDECODE` and `CLAUDE_CODE_*` are stripped from the agent's environment; a dev server that
never prints `Local:` is accepted when `server.url` answers; `@playwright/mcp` is pinned at
0.0.80 and driven over stdio by the harness itself for SMOKE; after a revert the repair prompt
gains one sentence saying the chain was reset.

```
hanvil run [SPEC] [--max-attempts N] [--new | --continue BRANCH] [--workspace DIR] [--no-skills]
hanvil doctor [SPEC] [--recipe-only]
hanvil validate [SPEC]              # ASSERT, then SMOKE when ASSERT is clean; no agent
hanvil validate-semantic [SPEC]     # EVALUATE only, against the workspace as it is
hanvil init [DIR] [--repo URL] [--ref REF] [--template NAME] [--skip-install]
```

The node flags apply to every subcommand. `run` binds 7546, 5551 and 50211 unless a flag or the
recipe's `chainValidation.local` says otherwise, so `Client.forLocalNode()` in the generated app
works untouched. `validate` and `validate-semantic` boot the same chain and provision a signer
for the app's dev server, and sweep it after; they do not run the recipe's deploy commands.
`run --continue` on a session that already passed starts the next cycle — a fresh GENERATE with
upstream's `continue` prompt — not a no-op. Artifacts land under `.harness/runs/<timestamp>-<name>/` in the upstream layout
— `session.json`, `status.json`, `prompts/`, `logs/`, `reports/report.json` — plus
`logs/chain-state-attempt-N.json`; the signer's private key reads `<redacted by hanvil>` in
every prompt file, and `.harness/runs/harness.log.jsonl` records every event with its timestamp
and, for the chain events, its `durationMicros`. Ctrl-C kills the agent, the dev server and the
browser by process group, writes `status.json` with `"phase": "interrupted"`, and exits 130.

## The TypeScript harness on hanvil

The upstream `hedera-harness` also runs against Hanvil, over the network, with no credentials.
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
 HAPI     :50211 ┤                    HAPI records · revm CacheDB
 hanvil run ─────┘                    (balances authoritative in tinybar)
   │ spawns: agent CLI · git · deploy commands · dev server · @playwright/mcp
```

`hanvil run` holds the same lock. The signer, the snapshot, the revert, the assertions and the
state dump are method calls on `Chain` under `write()` or `read()`, never across an `.await`;
the agent, `git`, the deploy commands, the dev server and the browser are subprocesses in their
own process groups, and the harness reads their pipes.

`revm` executes, `alloy` decodes and recovers senders, `axum` serves all three listeners, `tonic`
and `prost` speak HAPI over the 130 vendored protobuf files, `clap` reads the flags,
`serde_yaml_ng` reads the recipe, `regex` runs the secret scan. The node makes no outbound
network calls — it never fetches anything; the processes `hanvil run` spawns are listed under
[Harness](#harness). The build is laid out in `docs/code-plan.md`; every claim about how Hedera's
own tooling behaves is pinned to a file and line in `docs/research.md`.

MIT. Vendored HAPI protobufs are Apache-2.0; the harness's prompt templates, recipe schema,
skeletons and console strings derive from `hedera-dev/hedera-harness`, MIT — see NOTICE.
