# Code plan — Hanvil

Derived from `docs/research.md`. Facts there, decisions here. Dates and gates in `../plan.md`.

## 1. What Hanvil is, in one paragraph

One Rust binary. One in-memory state. Three listeners that speak the three protocols a Hedera
app already uses — JSON-RPC on 7546 (relay shape), REST on 5551 (mirror shape), gRPC on 50211
(consensus HAPI) — so a scaffold-hbar app, `@hiero-ledger/sdk`, hardhat, foundry and the harness
connect without knowing it is not hiero-local-node. Plus Anvil's cheats: deterministic accounts,
`evm_snapshot`/`evm_revert`, time travel, impersonation, instant start.

## 2. Crate layout

Single crate `hanvil`, binary target, modules. Split later if it earns it.

```
hanvil/
├── Cargo.toml
├── build.rs                  # tonic-prost-build over proto/services/*.proto
├── proto/services/*.proto    # vendored HAPI (119-file closure + google wrappers)
├── src/
│   ├── main.rs               # clap, boot, spawn 3 servers, banner
│   ├── cli.rs                # flags (§11)
│   ├── state/
│   │   ├── mod.rs            # Chain: the one struct, RwLock<Chain>
│   │   ├── accounts.rs       # Hedera id ↔ EVM address ↔ key; tinybar balances
│   │   ├── blocks.rs         # blocks, txs, receipts, logs, consensus timestamps
│   │   ├── entities.rs       # id allocator (0.0.N), contracts, topics, files
│   │   └── snapshot.rs       # clone-on-snapshot, revert
│   ├── evm/
│   │   ├── mod.rs            # revm Context/CacheDB wrapper, execute(tx) -> receipt
│   │   ├── units.rs          # tinybar<->weibar, long-zero addresses
│   │   └── precompiles.rs    # 0x167 HTS: v0 = revert with a clear message
│   ├── rpc/                  # JSON-RPC (axum + serde_json)
│   │   ├── mod.rs            # dispatch table
│   │   ├── eth.rs            # eth_* (§7)
│   │   ├── cheats.rs         # evm_*, anvil_* (§7)
│   │   └── types.rs          # hex quantities, block/tx/receipt JSON
│   ├── mirror/               # REST (axum)
│   │   ├── mod.rs            # router
│   │   ├── accounts.rs · transactions.rs · contracts.rs · topics.rs · blocks.rs · network.rs
│   │   └── shapes.rs         # exact field sets from openapi.yml (§8)
│   ├── hapi/                 # gRPC (tonic)
│   │   ├── mod.rs            # server, service registration
│   │   ├── wire.rs           # Transaction -> SignedTransaction -> TransactionBody; prechecks
│   │   ├── render.rs         # chain types back into receipts, records, infos
│   │   ├── crypto.rs         # CryptoService
│   │   ├── consensus.rs      # ConsensusService
│   │   ├── contract.rs       # SmartContractService (callEthereum + contractCallLocal)
│   │   ├── network.rs        # getVersionInfo
│   │   └── queries.rs        # QueryHeader/ResponseHeader, COST_ANSWER handling
│   └── keys/
│       ├── predefined.rs     # the 30 local-node keys + id assignment
│       └── sig.rs            # secp256k1 (keccak(bodyBytes)) + ed25519 verify
└── tests/
    ├── rpc.rs                # Rust: deploy+call via raw RPC
    ├── hapi.rs               # Rust: tonic client create/transfer/delete/receipt
    └── js/                   # Node: @hiero-ledger/sdk + viem against a running hanvil
        ├── package.json
        ├── sdk.test.mjs      # Client.forLocalNode() end to end
        └── viem.test.mjs     # deploy + call + logs
```

### 2b. Dependencies added after the plan was written

| Crate | Version | Why |
| --- | --- | --- |
| `base64` | 0.22 | The mirror's `format: byte` fields (`transaction_hash`, `memo_base64`, and Day 3's topic `message`) are base64. Already in the lock file as a transitive dependency; MIT/Apache-2.0. |

## 3. State model

```rust
pub struct Chain {
    cfg: Config,                         // chain_id, gas_price_tinybar, automine, sig_verify
    ids: IdAllocator,                    // next 0.0.N (starts 1002 after predefined)
    accounts: BTreeMap<EntityId, Account>,
    by_evm: HashMap<Address, EntityId>,  // both alias and long-zero addresses index here
    contracts: BTreeMap<EntityId, ContractMeta>,   // evm address, created_at, creator
    topics: BTreeMap<EntityId, Topic>,   // memo, admin/submit key, seq, running_hash, messages
    db: CacheDB<EmptyDB>,                // revm world state: code, storage, nonce; balances mirrored
    blocks: Vec<Block>,                  // one block per mined tx (automine) or evm_mine
    txs: HashMap<TxHash, TxRecord>,      // EVM txs: raw, receipt, logs, block idx
    hapi_txs: HashMap<TxId, HapiRecord>, // HAPI txs: body, receipt, record, consensus ts
    tx_index: Vec<TxRef>,                // consensus order across both kinds, for /transactions
    now_offset: i64,                     // evm_increaseTime
    next_timestamp: Option<i64>,
    impersonated: HashSet<Address>,
}
pub struct Account { id: EntityId, key: Option<Key>, alias: Option<Address>,
                     balance_tinybar: u64, nonce: u64, deleted: bool, memo: String,
                     max_auto_assoc: i32, created_ts: Timestamp, receiver_sig: bool }
```

Balance is authoritative in `Account.balance`; **the EVM's native unit is the tinybar** (as on
Hedera), so the revm `CacheDB` balance is written as tinybar, unscaled, before every EVM execution
and read back after (§5). One `RwLock<Chain>`; every request takes it — including `eth_call`,
which runs the EVM in place. Correctness first; contention is irrelevant at this scale.
Snapshots live inside `Chain` as `BTreeMap<u64, Chain>` and are taken out before the clone, so a
snapshot never nests earlier snapshots. Ids keep increasing across reverts, as Anvil's do.

## 4. Identity and units

- Entity id `0.0.N`. Predefined: 1002–1011 ECDSA, 1012–1021 alias-ECDSA, 1022–1031 ED25519,
  10,000 ℏ each, keys from `hiero-local-node/src/configuration/accountConfiguration.json`.
  Genesis system accounts 0.0.2 (treasury, the local-node operator key), 0.0.3 (node), 0.0.98
  (fees) exist with large balances. New ids allocate from 1032.
- Long-zero address for any entity: 20 bytes = shard(4 BE) ‖ realm(8 BE) ‖ num(8 BE).
- Alias address for ECDSA accounts = keccak256(uncompressed pubkey[1..])[12..].
  `by_evm` indexes both forms. `/accounts/{x}` accepts `0.0.N`, `0x…` long-zero, `0x…` alias.
- Contracts created by EVM `CREATE`/`CREATE2` get the standard EVM address and a fresh `0.0.N`;
  `created_contract_ids` and `contract_id` in mirror responses use the id.
- Units: **1 tinybar = 10¹⁰ weibar**, and the conversion happens only at the JSON-RPC boundary.
  `eth_getBalance` = tinybar × 10¹⁰; `eth_gasPrice` = tinybar price × 10¹⁰. Incoming `tx.value`
  must be a multiple of 10¹⁰ (`-32602` naming the rule otherwise); incoming gas prices are floored
  to whole tinybar, as the relay floors. Inside the EVM `msg.value`, balances and `gasprice` are
  tinybar — a Solidity `1 ether` literal is 10¹⁸ tinybar, the same quirk real Hedera has.
  Gas price default **71 tinybar per gas** (`--gas-price`), reported as 710 gwei. revm enforces
  `gas_price ≥ base fee` with `basefee = 71`, and the base fee it burns is credited back to
  0.0.98 after execution so total supply is conserved and fees are visible on that account.
  HAPI tx fee flat `--hapi-fee` (default 0), Day 3.

## 5. Execution paths

**EVM tx (`eth_sendRawTransaction`)** — built Day 1: decode with `alloy-consensus` (legacy,
2930, 1559; 4844 and 7702 refused by type); recover sender; convert value and gas price to tinybar;
sync every account's balance and nonce into `CacheDB`; `evm.transact` (revm checks chain id, nonce,
funds, gas price, block gas limit — each mapped to a `-32000` message naming the numbers); commit
the state map, read touched balances and nonces back, allocate `0.0.N` ids for created contracts,
create hollow accounts for fresh addresses that received value, credit the burned base fee to
0.0.98; append one block; store receipt and logs. `eth_sendTransaction` runs the same path unsigned
for predefined and impersonated senders (hash = keccak of a fixed preimage) and refuses anyone
else by address. A HAPI-side record (`transaction_id` = `0.0.<payer>-<sec>-<nanos>`, name
`ETHEREUMTRANSACTION`, result `SUCCESS`/`CONTRACT_REVERT_EXECUTED`) is Day 2's job so that
`/transactions` and `/contracts/results/{hash}` answer.
`eth_estimateGas` runs the call at the cap, then bisects between gas used and the cap for the
smallest passing limit (the 63/64 rule). Reverts from `eth_call`/`eth_estimateGas` return
`{code: 3, message: "execution reverted: <Error(string) text>", data: 0x…}`; halts return -32000.
Only the head state is served: a historical block tag returns -32000 naming the head.

**HAPI tx (gRPC)**: `Transaction.signedTransactionBytes` → `SignedTransaction` → `bodyBytes` →
`TransactionBody`. Checks in order, each mapping to a precheck code: node account is 0.0.3
(`INVALID_NODE_ACCOUNT`), `transactionID.validStart` within `[now−3min, now+1min]`
(`INVALID_TRANSACTION_START`), not already seen (`DUPLICATE_TRANSACTION`), payer exists
(`INVALID_ACCOUNT_ID`), signature valid for payer key unless `--no-sig-verify`
(`INVALID_SIGNATURE`), payer balance ≥ fee (`INSUFFICIENT_PAYER_BALANCE`). Return
`TransactionResponse{OK}` immediately, then apply the body synchronously and store the receipt
under the transaction id — so the SDK's first `getTransactionReceipts` already sees `SUCCESS`.

Body handlers in v0:
| Body | Effect | Receipt |
| --- | --- | --- |
| cryptoCreateAccount | new id; key; alias (must match key if ECDSA); initialBalance from payer; maxAutoAssoc | `accountID` |
| cryptoTransfer | HBAR `accountAmounts` must sum to 0; each debit needs sig of that account; credits to unknown alias → **hollow account** auto-create | — |
| cryptoDelete | mark deleted, move balance to `transferAccountID`; needs account sig | — |
| consensusCreateTopic | new topic id, memo, keys | `topicID` |
| consensusSubmitMessage | seq += 1; running_hash = sha384(prev ‖ topic ‖ ts ‖ seq ‖ msg) v3 | `topicSequenceNumber`, `topicRunningHash`, version 3 |
| ethereumTransaction | route to the EVM path (callEthereum) | `contractID` |
| contractCall / contractCreateInstance | out of v0: return `NOT_SUPPORTED` with message naming JSON-RPC | — |
| token* | out of v0: `NOT_SUPPORTED` | — |

Queries: `cryptoGetBalance` (free), `getAccountInfo` (paid: answer `COST_ANSWER` with cost 0,
accept `ANSWER_ONLY` with or without a payment tx in the header — **verify SDK flow Day 0**),
`getTransactionReceipts` (`RECEIPT_NOT_FOUND` if unknown), `getTxRecordByTxID`, `getTopicInfo`,
`contractCallLocalMethod` (→ `evm.transact` without commit), `getVersionInfo`.

Signature verification (`keys/sig.rs`): for each `SignaturePair`, match `pubKeyPrefix` against the
account key; ECDSA = secp256k1 over `keccak256(bodyBytes)`, 64-byte r‖s; ED25519 over
`bodyBytes`. Threshold/KeyList: v0 supports single keys only; a KeyList returns `INVALID_SIGNATURE`
with a message.

## 6. Blocks, time, snapshots

- Automine: every successful EVM tx mines one block. HAPI txs do not create EVM blocks but get a
  consensus timestamp and appear in `/transactions` and `/blocks` (block = all txs in the same
  second, matching the mirror's "block per record file" shape loosely — documented).
- `now = wall_clock + now_offset`; `evm_increaseTime`, `evm_setNextBlockTimestamp`,
  `evm_mine` as Anvil. `evm_setAutomine(false)` + `anvil_mine` batch.
- `evm_snapshot` → `Chain.clone()` stored under an incrementing hex id; `evm_revert(id)` swaps it
  back and drops later snapshots. `CacheDB` is `Clone`. Cost is O(state), fine for a dev chain.
  `anvil_dumpState`/`anvil_loadState` = serde of `Chain` (bincode) — Day 2 if cheap, else Day 6.

## 7. JSON-RPC surface

Implemented for real: `eth_chainId`, `eth_blockNumber`, `eth_getBalance`, `eth_getCode`,
`eth_getStorageAt`, `eth_getTransactionCount`, `eth_gasPrice`, `eth_maxPriorityFeePerGas` (0x0),
`eth_feeHistory` (flat), `eth_estimateGas` (see below), `eth_call`,
`eth_sendRawTransaction`, `eth_getTransactionByHash`, `eth_getTransactionReceipt`,
`eth_getBlockByNumber`, `eth_getBlockByHash`, `eth_getBlockReceipts`, `eth_getLogs` (address +
topics + block range), `eth_getBlockTransactionCountBy{Hash,Number}`,
`eth_getTransactionByBlock{Hash,Number}AndIndex`, `net_version`, `net_listening`,
`web3_clientVersion` ("hanvil/<ver>"), `web3_sha3`.
Relay-compatible stubs: `eth_accounts → []`, uncles → `null`/`0x0`, `eth_mining → false`,
`eth_hashrate → 0x0`, `eth_syncing → false`, `-32601` for the same set the relay rejects
(`research.md §5`). Cheats per `research.md §11`.
Errors: revert data returned as `{code:3, message:"execution reverted", data:0x…}` (Anvil/geth
shape) so viem and ethers decode custom errors.

`eth_estimateGas` deviates from the "+10 %" this plan first specified. It bisects between the gas
the unconstrained run reported and the block limit. The flat margin is wrong for any call that
makes a call: EIP-150 forwards at most 63/64 of the remaining gas, so the caller must hold gas the
callee never spends, and the reported `gas_used` can be well under the limit the transaction needs
to pass. A margin large enough for a deep call chain would waste gas on a plain transfer. The
bisection asks the question directly — the smallest limit at which the call still succeeds — and
returns the block limit when even that fails, so a caller sees a gas error rather than a silent
underestimate.

## 8. Mirror REST surface

Exact required-field sets from `openapi.yml` (`research.md §6`), snake_case, timestamps as
`"sec.nanos"` strings, ids as `0.0.N`, amounts in tinybar, `links.next: null`. Endpoints:
`/api/v1/accounts/{id|alias|evm}` (+`?transactions=false`), `/accounts/{id}/tokens` (empty),
`/transactions` (`account.id`, `transactiontype`, `limit`, `order`, `timestamp`),
`/transactions/{0.0.x-sss-nnn}`, `/contracts/results/{hash|txid}`, `/contracts/{id}/results`,
`/contracts/{id}`, `/contracts/results/logs`, `/blocks`, `/blocks/{n|hash}`, `/topics/{id}`,
`/topics/{id}/messages` (`sequencenumber`, `limit`), `/network/nodes` (one node 0.0.3),
`/network/exchangerate` (fixed 1 ℏ = 12 ¢, matches local-node), `/network/fees`.
404 body `{"_status":{"messages":[{"message":"Not found"}]}}` — PR #39 keys on that shape.

## 9. gRPC surface

tonic services: `CryptoService`, `ConsensusService`, `SmartContractService` (only
`contractCallLocalMethod` + `callEthereum`; others `NOT_SUPPORTED`), `NetworkService`
(`getVersionInfo`). Plaintext h2c on 50211. Node account `0.0.3`.

## 10. HTS system contract (`0x167`)

v0, done 2026-09-09: **etched bytecode**, not a precompile — genesis writes a stub at `0x167`
that reverts with `Error(string)` carrying `"hanvil: HTS system contract not emulated; see
README#hts"`, so a call fails loudly instead of silently succeeding. Etching rather than a
precompile because `MainnetEvm` fixes the provider type to `EthPrecompiles`; a custom
`PrecompileProvider` means reconstructing `Evm` around new generics for no observable difference,
and etched code is part of the state a snapshot clones. scaffold's default deploy
skips HTS on any network not named `hardhat`/`localhost` (`research.md §7`); recipes name the
network `hanvil`. Stretch (only after G3): etch `hedera-forking`'s `HtsSystemContract` at `0x167`
and a revm inspector that etches the HIP-719 proxy at each address `createFungibleToken` returns.

## 11. CLI

```
hanvil [--port 7546] [--mirror-port 5551] [--grpc-port 50211] [--host 127.0.0.1]
       [--chain-id 298] [--accounts 10] [--balance 10000] [--gas-price <tinybar>]
       [--block-time <s>] [--no-sig-verify] [--state <file>] [--dump-state <file>] [--silent]
```
`--chain-id 31337 --port 8545` makes Hanvil a drop-in for scaffold's `hederaLocalFork` (hardhat
chain); `--chain-id 296` satisfies a hardhat config that hardcodes testnet's id.

Banner (stdout, once):
```
hanvil 0.1.0 — local Hedera network
JSON-RPC   http://127.0.0.1:7546   chain id 298
Mirror     http://127.0.0.1:5551
gRPC       127.0.0.1:50211         node 0.0.3

Accounts (ECDSA, 10000 ℏ each)
0.0.1002  0x…(long-zero)  0x7f109a9e…80d6
…
Accounts (ECDSA alias)
0.0.1012  0x67D8d32E…Ee69  0x105d0501…1524
…
Started in 38 ms
```

## 12. Tests

- Rust unit: units, long-zero, sig verify vectors (one ECDSA, one ED25519 body signed by the SDK
  and checked in as fixtures), snapshot/revert, running hash v3.
- Rust integration (`tests/`): boot on random ports; JSON-RPC deploy+call+logs; tonic client
  create→transfer→balance→delete→receipt.
- Node integration (`tests/js/`): `@hiero-ledger/sdk` `Client.forLocalNode()` runs the exact
  harness sequence (`setECDSAKeyWithAlias`, `AccountBalanceQuery`, `TransferTransaction`,
  `AccountDeleteTransaction`); viem deploys a contract and reads logs; mirror `/accounts/{evm}`
  resolves the alias PR #39 waits on.
- CI (`.github/workflows/ci.yml`): `cargo test`, then `node tests/js` against a built binary,
  then `hedera-harness@next` Tier 3.5 run on a trivial recipe with `network: local` — no secrets.

## 13. Harness PRs — against `dev`

**PR 1 `feat(chain): network "local" for Tier 3.5`** (files and lines from `research.md §1`):
`src/types.ts:155,168,388` widen to `"testnet" | "local"`, add
`local?: { rpcUrl; grpcUrl; mirrorUrl }`; `src/specLoader.ts:553,598`; `src/specDefaults.ts`
defaults `http://localhost:7546` / `localhost:50211` / `http://localhost:5551`;
`src/validation/chainSigner.ts` — `clientFor(config)`: local → `sdk.Client.forLocalNode()` when
URLs are defaults, else `Client.forNetwork({grpcUrl: AccountId(3)})` + `setMirrorNetwork`;
provisioning on local: operator = `0.0.1002` with its predefined key when env vars absent (no
portal), still creates an ephemeral alias account so the rest of the flow is identical; sweep
stays; `src/doctor.ts checkChainEnv`: on local, TCP-probe the three ports, name Hanvil /
hiero-local-node in the fix text; `prompts/validator.md:14-15,35-36` → `{{signerNetwork}}` and a
new `{{mirrorBaseUrl}}`; `src/promptBuilder.ts:231` wording; `docs/authoring-a-recipe.md`,
`.env.example`; `test/spec-schema.test.mjs` + `test/chain-local.test.mjs`.

**PR 2 `feat(chain): snapshot chain state per repair attempt`** (stacked): `src/attemptLoop.ts`
before `:180` → `evm_snapshot` via the local RPC; after a failed attempt → `evm_revert`;
record the snapshot id in the attempt artifacts; no-op with a log line on testnet;
`test/attempt-snapshot.test.mjs` with a fake RPC.

Both: rebase on `dev` daily; PR #39 and #15 touch `chainSigner.ts` — read before editing.

## 13b. Red-team findings folded in (2026-09-07)

- **Deploy target — resolved 2026-09-07 by running `init`.** The harness's default template,
  `templates/hedera-demo`, ships with `solidityFramework: none` and only `packages/nextjs`; its
  own outro says "no contract deploy is required". It talks to Hedera through native services —
  `@hiero-ledger/sdk` over gRPC and the mirror REST — which is precisely Hanvil's surface. The
  dogfood recipe uses this template; no hardhat network entry is needed. The scaffold-hbar
  `hederaLocal` PR remains a Day 5 extra for the Solidity flavors. Third contribution,
  Day 5 only if G3 passed: scaffold-hbar PR `feat: hederaLocal network` — hardhat network
  `{ url: HEDERA_LOCAL_RPC_URL ?? http://127.0.0.1:7546, chainId: 298 }`, nextjs chain 298 with
  `NEXT_PUBLIC_HEDERA_LOCAL_RPC_URL`, mirror `HEDERA_MIRROR_LOCAL_URL`.
- **PR 1 scope statement.** Local mode is for the repair loop; a verdict meant for publication
  still runs against testnet. The PR description states this so the tier's purpose is preserved.
- **PR 2 sentence.** `runChainDeploy` runs every attempt; contracts and HCS topics accumulate; the
  validator can pass an assertion on attempt-1 topic messages. Snapshot/revert removes that false
  positive.
- **Claims discipline.** "Works with hiero-local-node" is by construction (same ports, node 0.0.3)
  until it has been run against one; the README says which.
- **Budget.** Days 4–5 need ≥ 3 full harness runs (40 min–2 h each, agent tokens). Plan them.
- **Day 0 checks added:** flavor of `templates/hedera-demo`; behaviour of
  `scripts/runHardhatDeployWithPK.ts` with an unknown `--network`; SDK paid-query flow.

## 14. Build order and what each day proves

| Day | Modules | Proof |
| --- | --- | --- |
| 0 | repo, protos vendored, `build.rs` compiles, `state`, `keys/predefined`, `rpc` skeleton | `eth_chainId`, `eth_getBalance` for 0.0.1012 |
| 1 | `evm`, `rpc/eth` complete, `cheats` | `forge create` + call on Hanvil; snapshot/revert |
| 2 | `mirror/*` | `curl /accounts/0x67D8…` and `/contracts/results/{hash}` match shapes |
| 3 | `hapi/*`, `keys/sig` | SDK `Client.forLocalNode()` create → transfer → delete |
| 4 | PR 1, CI workflow | `hedera-harness@next run` Tier 3.5 on Hanvil, no env vars |
| 5 | PR 2, dogfood, testnet x402 | independent retries; paid request tx hash |
| 6 | README, video, form | submitted by 18:00 IST |

## 15. Non-goals (README carries this list verbatim)

Consensus, gossip, multi-node, staking, HTS/HFS/scheduled transactions (beyond `NOT_SUPPORTED`
answers), KeyList/threshold signatures, fee schedules and exchange-rate fidelity, HFS-based large
contract deploys, state forking from testnet/mainnet, mirror gRPC (5600), relay WebSocket (8546),
block-node, persistence across restarts unless `--state` is given.
