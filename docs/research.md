# Research — verified facts, with sources

Read 2026-09-07 from clones under `research/` (gitignored, 14 repos). Every line names the file it
came from. Anything not read is marked **verify**. Line numbers are from the clones at that date.

## 0. Sources on disk

| Dir | Repo | What was pulled |
| --- | --- | --- |
| `research/hedera-harness` | hedera-dev/hedera-harness `master` (v1.2.2) | full |
| `research/hedera-harness-dev` | same, `dev` (v2.0.0-rc.4) | full — **the branch PRs target** |
| `research/harness-prs/` | PR diffs #12 #15 #16 #39 #40, open issues JSON | |
| `research/hiero-local-node` | hiero-ledger/hiero-local-node | full |
| `research/hiero-sdk-js` | hiero-ledger/hiero-sdk-js | `src/` |
| `research/hiero-json-rpc-relay` | hiero-ledger/hiero-json-rpc-relay | `docs/` |
| `research/hiero-mirror-node` | hiero-ledger/hiero-mirror-node | `rest/api/v1/openapi.yml` |
| `research/hiero-consensus-node` | hiero-ledger/hiero-consensus-node | `hapi/.../proto` (208 files) |
| `research/hiero-sdk-rust` | hiero-ledger/hiero-sdk-rust | full (protos are a submodule, 3 files only) |
| `research/scaffold-hbar` | hedera-dev/scaffold-hbar `main` | full |
| `research/hedera-forking` | hashgraph/hedera-forking | full |
| `research/x402` | x402-foundation/x402 | `specs/`, `typescript/packages/` |
| `research/x402-poc` | hedera-dev/x402-inference-pay-per-request-poc | full |
| `research/hedera-skills` | hedera-dev/hedera-skills | full |
| `research/foundry` | foundry-rs/foundry | `crates/anvil/src/eth/api.rs` |
| `research/revm` | bluealloy/revm | `examples/` |

## 1. hedera-harness

**Two live lines.** `master` = v1.2.2 = npm `latest`. `dev` = v2.0.0-rc.4 = npm `next`
(published 2026-09-03), schemaVersion 3. PR #12 "Dev" is `dev → master`, +3358/−4275, 103 files,
mergeable, opened 2026-08-26, updated 2026-09-04. It rewrites `types.ts`, `specLoader.ts`,
`chainSigner.ts`, `attemptStages.ts`, `sessionRunner.ts`, `session.ts`, `doctor.ts`,
`specDefaults.ts`. `hedera-skills/plugins/hedera-harness` already targets v3 and says `migrate`
is gone. **All PRs go against `dev`.** A PR on master is obsolete the day #12 merges.

**The seam on `dev`** (`research/hedera-harness-dev`):
- `src/types.ts:155` `network: "testnet"` on `ChainValidationConfig`; `:168` on `ChainSigner`;
  `:388` on the `chain_signer_provisioned` log event; `:151` doc comment.
- `src/specLoader.ts:553` throws unless `"testnet"`; `:598` returns the literal.
- `src/validation/chainSigner.ts:69,118,149,202` `sdk.Client.forTestnet()`; `:94,:420` literal.
- `src/doctor.ts` `checkChainEnv` demands the two operator env vars with fix text
  "Testnet credentials from https://portal.hedera.com" (master `:351-366`; **verify line on dev**).
- `prompts/validator.md:14-15` "funded disposable testnet account"; `:35-36` mirror base URL
  `https://testnet.mirrornode.hedera.com` hardcoded into the validator agent's prompt.
- `src/promptBuilder.ts:231` "verified on the Hedera testnet mirror node".
- `src/evalInfra.ts:39,46` outage regexes mention testnet (classification only; leave).
- Attempt loop: `src/attemptLoop.ts:180` calls `runGenerateStage`, `:194` `runValidationStages`.
  **PR 2's snapshot/revert hook lives here.** (On master it was inside `sessionRunner.ts`.)
- Signer provisioned once per run in `sessionRunner.ts` (master `:177-197`), persisted to
  `runs/<id>/chain-signer.json` mode 0600, reused across attempts and `--continue`; swept
  (AccountDelete → operator) at run end. Exported to deploy commands as
  `HARNESS_SIGNER_ACCOUNT_ID / _EVM_ADDRESS / _PRIVATE_KEY` (`chainSigner.ts buildDeployEnv`).
- Browser injection: validator agent runs `localStorage.setItem("burnerWallet.pk", <hex>)`
  (`prompts/validator.md:20`), key name from `expose.browserLocalStorageKey`.
- Every SDK call in Tier 3.5 (master `chainSigner.ts`): `PrivateKey.generateECDSA()`,
  `publicKey.toEvmAddress()`, `AccountCreateTransaction().setECDSAKeyWithAlias(key)
  .setInitialBalance(Hbar)` → `getReceipt`, `AccountBalanceQuery`, `TransferTransaction
  .addHbarTransfer ×2` → `getReceipt`, `AccountDeleteTransaction.setTransferAccountId` → sign →
  `getReceipt`. PR #15 adds `.setMaxAutomaticTokenAssociations(-1)` and an HBAR drain path.
- `dev` src: 10,291 lines. No `src/validation/mirrorNode.ts` on dev — PR #39 (mirror reader,
  905-line diff, adds `waitForAccount`, `waitForTransaction`, tx-id normalisation
  `0.0.x@sss.nnn → 0.0.x-sss-nnn`) targets master and is unmerged on both lines.
- Tests: `node --test test/*.test.mjs` after `tsc` build; CI (`.github/workflows/ci.yml`):
  Node 20, `npm ci`, `npm test`, `npm run typecheck`, `npm run build`, `npm pack --dry-run`,
  `smoke:pack`. A PR ships a `test/<name>.test.mjs`.
- Shipped PRDs: `docs/prds/x402-metered-api.md` (testnet + Blocky402 + HCS receipt topic,
  burner-wallet mode = the Tier 3.5 signer), `hts-precompile-demo.md`, `hedera-proof-wall-demo.md`.

## 2. hiero-local-node (the incumbent)

`README.md:49` "Minimum 16GB RAM"; `:60-62` Docker 8 GB memory, 1 GB swap, 64 GB disk.
`.env`: consensus-node image `0.72.0` with `NETWORK_NODE_MEM_LIMIT=8gb`; mirror `0.151.0`; relay
`0.75.0`. Compose (`docker-compose.yml`) runs: haveged, network-node, three uploaders, minio,
postgres, mirror grpc/importer/rest/rest-java/web3/monitor, nginx api-proxy, explorer, block-node,
cadvisor, relay, relay-ws, envoy. Startup waits on ports 5600 then 50211 with up to 100 retries
(`src/services/ConnectionService.ts:59`, `src/state/StartState.ts:96-97`).

Ports (`README.md:586-594`): consensus gRPC **50211** (TLS 50212), mirror gRPC **5600**, mirror
REST **5551**, relay **7546**, relay WS 8546, explorer 8090, block node 8080, web3 8545.
Chain id **298 = 0x12a** (`.env:58 RELAY_CHAIN_ID`, relay `docs/configuration.md:159`: local and
previewnet 0x12a, testnet 0x128 = 296, mainnet 0x127 = 295). Relay operator `0.0.2`; network map
`{"network-node:50211":"0.0.3"}`.

Predefined accounts (`src/configuration/accountConfiguration.json`, 10 each): ECDSA
→ `0.0.1002–1011`; alias-ECDSA (have EVM addresses) → `0.0.1012–1021`, first is
`0x67D8d32E9Bf1a9968a5ff53B87d777Aa8EBBEe69` / key `0x105d05…1524`; ED25519 → `0.0.1022–1031`.
10,000 ℏ each (`README.md:192,208,224`). Hanvil ships these exact keys and ids.

The README's hardhat snippet (`README.md:466-482`) is the shape Hanvil's README repeats:
`local: { url: 'http://localhost:7546', accounts: [0x105d05…, 0x2e1d96…], chainId: 298 }`.

## 3. hiero-sdk-js (what talks to Hanvil's gRPC)

- `Client.forLocalNode()` exists on the Node client (`src/client/NodeClient.js:191`), network name
  `"local-node"` → `127.0.0.1:50211 → AccountId(3)` (`src/constants/ClientConstants.js:92`), mirror
  `["127.0.0.1:5600"]` (`:226`). `NativeClient.js:185` skips the address-book update for
  local-node, so nothing hits mirror gRPC. `Client.forNetwork({host:port: AccountId})` also works;
  `Client.js:319` treats any `127.0.0.1`/`localhost` key as local.
- Channel is plaintext `credentials.createInsecure()` on 50211 (`src/channel/NodeChannel.js:118`).
- gRPC methods per SDK class: `AccountCreateTransaction → crypto.createAccount`;
  `AccountDeleteTransaction → crypto.cryptoDelete`; `TransferTransaction → crypto.cryptoTransfer`
  (`src/account/TransferTransaction.js`); `AccountBalanceQuery → crypto.cryptoGetBalance`;
  `AccountInfoQuery → crypto.getAccountInfo`; `TransactionReceiptQuery →
  crypto.getTransactionReceipts`; `TransactionRecordQuery → crypto.getTxRecordByTxID`.
- Wire: `Transaction{signedTransactionBytes}` → `SignedTransaction{bodyBytes, sigMap}` →
  `TransactionBody` (`services/transaction_contents.proto:40,49`). Signatures are over `bodyBytes`.
- Receipt polling (`src/transaction/TransactionReceiptQuery.js:209-214`): retries on
  `BUSY`, `UNKNOWN`, `RECEIPT_NOT_FOUND`, `PLATFORM_NOT_ACTIVE`; finishes on `OK`/`SUCCESS`.
  Precheck read from `TransactionResponse.nodeTransactionPrecheckCode`
  (`src/transaction/Transaction.js:2245`).
- Transaction id text `0.0.x@sss.nnn` (`src/transaction/TransactionId.js:153`), valid duration
  default 120 s (`:45`).
- `AccountCreateTransaction.setECDSAKeyWithAlias` sets both `key` and 20-byte `alias`
  (`src/account/AccountCreateTransaction.js:316-321`).
- **verify Day 0:** how paid queries (`AccountInfoQuery`) do COST_ANSWER then payment
  (`src/query/Query.js`), so Hanvil answers cost 0 and accepts an unpaid ANSWER_ONLY.

## 4. HAPI protobufs

Path: `research/hiero-consensus-node/hapi/hedera-protobuf-java-api/src/main/proto/services/`.
Import closure for the four services Hanvil serves is **119 files** plus
`google/protobuf/wrappers.proto` — vendor the whole `services/` dir; `prost-build` needs `protoc`
(use `protoc-bin-vendored`).

Services/rpcs (`crypto_service.proto`, `consensus_service.proto`,
`smart_contract_service.proto`, `network_service.proto`):
- CryptoService: createAccount, updateAccount, cryptoTransfer, cryptoDelete, approveAllowances,
  deleteAllowances, getAccountRecords, cryptoGetBalance, getAccountInfo, getTransactionReceipts,
  getTxRecordByTxID.
- ConsensusService: createTopic, updateTopic, deleteTopic, submitMessage, getTopicInfo.
- SmartContractService: createContract, updateContract, contractCallMethod,
  contractCallLocalMethod, getContractInfo, ContractGetBytecode, deleteContract, callEthereum.
- NetworkService: getVersionInfo, getAccountDetails.

`TransactionBody` oneof numbers (`transaction.proto`): contractCall 7, contractCreateInstance 8,
cryptoCreateAccount 11, cryptoDelete 12, cryptoTransfer 14, consensusCreateTopic 24,
consensusSubmitMessage 27, tokenCreation 29, tokenAssociate 40, ethereumTransaction 50.

`ResponseCodeEnum` (`response_code.proto`): OK 0, INVALID_NODE_ACCOUNT 3, TRANSACTION_EXPIRED 4,
INVALID_TRANSACTION_START 5, INVALID_SIGNATURE 7, INSUFFICIENT_TX_FEE 9,
INSUFFICIENT_PAYER_BALANCE 10, DUPLICATE_TRANSACTION 11, BUSY 12, INVALID_ACCOUNT_ID 15,
RECEIPT_NOT_FOUND 18, UNKNOWN 21, **SUCCESS 22**, INSUFFICIENT_GAS 30,
CONTRACT_REVERT_EXECUTED 33, ACCOUNT_DELETED 72, WRONG_NONCE 312.

Fields: `TransactionReceipt` status 1, accountID 2, contractID 4, exchangeRate 5, topicID 6,
topicSequenceNumber 7, topicRunningHash 8, topicRunningHashVersion 9, newTotalSupply 11.
`CryptoCreateTransactionBody` key 1, initialBalance 2, receiverSigRequired 8, autoRenewPeriod 9,
memo 13, alias 18. `TransactionGetReceiptQuery` header 1, transactionID 2, includeDuplicates 3;
response header 1, receipt 2. `TransactionResponse` nodeTransactionPrecheckCode 1, cost 2.

## 5. JSON-RPC relay (what Hanvil's :7546 must look like)

The relay is **mirror-node-backed**: reads are served from the mirror REST; only
`eth_sendRawTransaction` reaches consensus, wrapped as a HAPI `EthereumTransaction`
(`docs/configuration.md:47,66,78`). Hanvil collapses relay + mirror + consensus into one process.

66 methods in `docs/openrpc.json`. Per `docs/rpc-api.md`, these exist but return `-32601`:
`eth_blobBaseFee`, `eth_coinbase`, `eth_createAccessList`, `eth_getProof`, `eth_protocolVersion`,
`eth_sendTransaction`, `eth_sign`, `eth_signTransaction`, `eth_signTypedData`, `eth_getWork`,
`eth_submitHashrate`. `eth_accounts → []`, uncles → `null`/`0x0`, `eth_hashrate → 0x0`,
`eth_mining → false`, `eth_syncing → false`, `eth_maxPriorityFeePerGas → 0x0`.
`eth_gasPrice` = network tinybar gas price converted to wei (`rpc-api.md:87`).
Units: HBAR has 8 decimals; JSON-RPC uses 18 → **1 tinybar = 10¹⁰ wei ("weibar")**
(scaffold `scaffold.config.ts:16-19` comments the same). Contract bytecode > 24 KB goes through
HFS file append (`configuration.md:61-62,78`) — Hanvil ignores that path.

## 6. Mirror node REST (what Hanvil's :5551 must look like)

`rest/api/v1/openapi.yml`. Endpoints Hanvil serves (the harness, PR #39, `@x402/hedera`
preflight and the validator prompt use these): `/api/v1/accounts/{idOrAliasOrEvmAddress}`,
`/accounts/{id}/tokens`, `/transactions`, `/transactions/{transactionId}`,
`/contracts/results/{transactionIdOrHash}`, `/contracts/{id}/results`, `/contracts/results/logs`,
`/contracts/{id}`, `/blocks`, `/blocks/{hashOrNumber}`, `/topics/{topicId}`,
`/topics/{topicId}/messages`, `/network/nodes`, `/network/exchangerate`, `/network/fees`.

Required fields: **AccountInfo** account, alias, auto_renew_period, balance{balance,timestamp,
tokens}, created_timestamp, decline_reward, delegation_address, deleted, ethereum_nonce,
evm_address, expiry_timestamp, key{_type,key}, max_automatic_token_associations, memo,
receiver_sig_required, staked_account_id, staked_node_id, stake_period_start.
**Transaction** charged_tx_fee, consensus_timestamp, entity_id, name, node, nonce, result,
transaction_hash, transaction_id, transfers[], valid_start_timestamp, valid_duration_seconds.
**ContractResult** address, amount, block_hash, block_number, call_result, contract_id,
created_contract_ids, from, gas_limit, gas_used, hash, result, status, timestamp, to, logs.
**TopicMessage** consensus_timestamp, message (base64), payer_account_id, running_hash,
running_hash_version, sequence_number, topic_id. Transaction id in URLs is `0.0.x-sss-nnn`.

## 7. scaffold-hbar (what the harness builds against)

`packages/{hardhat,foundry,nextjs}`. Template branches: blank-template, bridge, cross-chain-dca,
**hedera-demo**, oracles, payments-scheduler, tokenize-subscriptions, **x402-pay-per-use**.

- Frontend: viem/wagmi. `scaffold.config.ts:11-22` defines `hederaLocalFork = {...chains.hardhat,
  HBAR 18 decimals}` → **chain id 31337, RPC 127.0.0.1:8545** — scaffold's "local" is hardhat, not
  hiero-local-node. `utils/scaffold-hbar/hederaAccountId.ts:5-8` maps 295/296 only; anything else
  defaults to "testnet". Mirror URLs in `app/api/hedera/account/route.ts:4-5` override via
  `HEDERA_MIRROR_TESTNET_URL` / `_MAINNET_URL`. RPC override `NEXT_PUBLIC_HEDERA_TESTNET_RPC_URL`.
- Hardhat (`hardhat.config.ts:49-67`): networks `hardhat` (forking testnet via
  `@hashgraph/system-contracts-forking` when `HEDERA_FORKING=true`, `hardhat.config.ts:13-16`),
  `hederaTestnet` 296, `hederaMainnet` 295. `yarn chain` = `HEDERA_FORKING=true hardhat node`.
  Deployer default key is Anvil/Hardhat account 0 (`:27`).
- `deploy/02_create_hts_token.ts:7` calls `HtsTokenCreator.createToken` (HTS system contract
  `0x167`) **only when the network is named `hardhat` or `localhost`**; every other network name
  skips it. A network entry named `hanvil` therefore deploys without HTS.
- Burner wallet: `burner-connector`, key in `localStorage["burnerWallet.pk"]`
  (`components/.../SetBurnerPKModal.tsx:8`).
- Foundry `foundry.toml:17` `localhost = http://127.0.0.1:8545`, `evm_version = "cancun"`.

## 8. hedera-forking (HTS emulation)

`@hashgraph/system-contracts-forking` 0.1.2 = `hashgraph/hedera-forking`.
`contracts/HtsSystemContract.sol` is 1,061 lines of Solidity implementing createFungibleToken,
mintToken, associateToken(s), transferToken(s), getTokenInfo, ERC-20/721 views. It is **not
self-contained in the EVM**: the Hardhat plugin hooks `eth_getCode` and `eth_getStorageAt`
(`src/plugin/index.js:89`, `src/forwarder/json-rpc-forwarder.js:58,87`) to serve the HIP-719
proxy bytecode for token addresses and to pull token storage from the mirror node; Foundry uses
`vm.etch` via `Hsc.sol`. README `:25-26`: "SHOULD BE ONLY used to ease development … DOES NOT
replicate HTS fully." Porting it into Hanvil means etching `HtsSystemContract` at `0x167` in revm
and etching the HIP-719 proxy at each address it creates — a revm inspector hook. Feasible, not
v0.

## 9. x402 on Hedera

- Spec `x402/specs/schemes/exact/scheme_exact_hedera.md`: client builds a `TransferTransaction`
  with `transactionId.accountId = extra.feePayer` (the facilitator), signs; resource server →
  facilitator `/verify` → `/settle`; facilitator co-signs as fee payer and submits. Verification
  fetches the payer key via consensus `AccountInfoQuery` (`:154`). Networks are CAIP-2
  `hedera:mainnet` / `hedera:testnet` (`:130`).
- `@x402/hedera` (`x402/typescript/packages/mechanisms/hedera/src/constants.ts`):
  `SUPPORTED_HEDERA_NETWORKS = [mainnet, testnet]` — **no local network id**; preflight reads
  mirror `/accounts/{id}` and `/accounts/{id}/tokens` with an overridable `mirrorNodeUrl`.
- Facilitators: `api.blocky402.com/supported` (fetched 2026-09-07) lists **`hedera:mainnet` only**,
  feePayer `0.0.10571514`. The PoC (`x402-poc/README.md:32-33`) routes testnet to
  `x402.org/facilitator` and mainnet to Blocky402. scaffold's `templates/x402-pay-per-use`
  ships a **self-hosted facilitator** (`services/x402/server.ts:11-20`, `FACILITATOR_URL`
  default `http://localhost:4020`, `X402_NETWORK` default `hedera:testnet`).
  **Open:** the prize text says "settled through the Blocky402 facilitator" — on testnet that may
  not exist. Ask in Hedera Discord on Day 0; fallback is the self-hosted facilitator on testnet,
  which is what Hedera's own template does.
- Harness PRD flow (`docs/prds/x402-metered-api.md:36-113`): burner mode reads
  `localStorage["burnerWallet.pk"]`, partial-signs with `@x402/hedera`, HCS receipt topic via SDK
  `setOperator` + `execute`; receipts read from mirror `/topics/{id}/messages`. Local run of this
  PRD needs ConsensusService + topic endpoints in Hanvil and a facilitator that accepts a local
  network — the latter is out of v0 scope, so the local dogfood uses a non-x402 PRD.

## 10. hedera-skills

`plugins/`: agent-kit-plugin, cross-chain, dev-intelligence, hackathon-helper, **hedera-harness**
(create/review-harness-spec, targets schemaVersion 3), hiero-cli (`localnet` network name),
native-services-js, oracles, system-contracts. `hackathon-helper/skills/*/SKILL.md` is Hedera's
own rubric: Innovation 10 %, Feasibility 10 %, **Execution 20 %**, Integration 15 % (mirror-only
= 1/5; multiple services = 3–4/5), Validation 15 %, **Success 20 %** (drives account creation /
TPS), Pitch 10 %. README doubles as the pitch. Use it to shape the README on Day 6.

## 11. Anvil (reference surface)

`foundry/crates/anvil/src/eth/api.rs` method list. The subset Hanvil mirrors: `evm_snapshot`,
`evm_revert`, `evm_mine`, `evm_increaseTime`, `evm_setNextBlockTimestamp`, `evm_setAutomine`,
`anvil_setBalance`, `anvil_setCode`, `anvil_setNonce`, `anvil_setStorageAt`,
`anvil_impersonateAccount`, `anvil_stopImpersonatingAccount`, `anvil_mine`, `anvil_reset`,
`anvil_dumpState`, `anvil_loadState`, `anvil_nodeInfo`, `web3_clientVersion`.

## 12. Rust stack (crates.io, 2026-09-07)

revm 43.0.0 (API: `Context::mainnet().with_db(CacheDB::<EmptyDB>::default())`, `build_mainnet()`,
`transact_commit(TxEnv::builder()…build())`, `transact(…)`; traits `ExecuteCommitEvm`,
`ExecuteEvm`, `MainBuilder`, `MainContext` — `revm/examples/contract_deployment/src/main.rs`),
tonic 0.14.6 (+ `tonic-prost-build`), prost 0.14.4, axum 0.8.9, tokio 1.53.1,
alloy-primitives 1.7.2, alloy-consensus 2.4.1, alloy-rlp 0.3.16, k256 0.14.0, clap 4.6.6.
**verify Day 0:** revm 43's pinned `alloy-primitives` — match it, do not pick independently.

## 13. Verify on Day 0 (each is one command)

1. `dev` `doctor.ts` line for `checkChainEnv`; confirm `attemptLoop.ts:180/194` after a fresh pull.
2. SDK paid-query flow (`hiero-sdk-js/src/query/Query.js`): COST_ANSWER → payment → ANSWER_ONLY.
3. revm 43 `Cargo.toml` alloy pins (`cargo tree` after `cargo add revm@43`).
4. Blocky402 testnet: Discord or `curl https://api.blocky402.com/supported` again.
5. `npx hedera-harness@next doctor` + one trivial run on testnet — the harness itself works.

## 14. Day 0 findings (2026-09-07)

- `npx hedera-harness@next init smoke-app --template hedera-demo` completes on this machine
  (yarn 3.2.3 via `corepack enable`; install 163 s). `doctor` is all green: recipe schema v3,
  `claude` agent on PATH, bundled prompts. The harness runs; the stop-condition is cleared.
- Toolchain: Rust 1.98.1 installed via rustup. revm 43.0.0 requires Rust ≥ 1.91; `rust-version`
  is 1.91. alloy-primitives 1.7.2 pins `k256 0.13.4`; hanvil uses the same so one copy is built.
  `tonic-prost` 0.14.6 needs ≥ 1.88.
- Proto tree: 130 files after adding `services/auxiliary/**`, `services/state/{hints,history}/*_types.proto`
  and `platform/event/state_signature_transaction.proto`. All 421 non-google imports are
  `services/…`-relative, so one include root (`proto/`) compiles without shadowing.
- Machine: 10 cores, 16 GB RAM — exactly hiero-local-node's stated minimum; Docker present.
  "Works with hiero-local-node" stays "by construction" unless a run is attempted with everything
  else closed.
- `templates/hedera-demo` (the harness's default `init` template): `template.json` declares
  `solidityFramework: none`; the tree is `packages/nextjs` only. The recipe skeleton's
  `chainValidation` block (v3) is `enabled / network: testnet / operator: {accountIdEnv,
  privateKeyEnv}` with the comment "Testnet only." — PR 1 also edits that skeleton comment.
- SDK paid queries, settled (`hiero-sdk-js/src/query/Query.js:295-350`): when `_isPaymentRequired()`
  (true by default; false for `AccountBalanceQuery` and `TransactionReceiptQuery`) the SDK first
  calls `getCost()` — a `COST_ANSWER` round-trip — then **always** builds a `CryptoTransfer`
  payment for that cost, even zero, signs it with the operator and attaches it as
  `QueryHeader.payment` on the `ANSWER_ONLY` request. Hanvil therefore answers `COST_ANSWER`
  with `cost: 0` and, on `ANSWER_ONLY`, decodes the payment and ignores it. No special casing.

## 15. Day 1 findings (2026-09-07, started a day early)

- **The EVM runs in tinybar on Hedera; the relay scales at the boundary.** scaffold's
  `packages/nextjs/scaffold.config.ts:16-19` says it in a comment (HBAR 18 decimals only on the
  RPC side) and `rpc-api.md:87` says `eth_gasPrice` is "tinybars converted to wei". Hanvil follows:
  revm balances, `msg.value` and `gasprice` are tinybar; `evm/units.rs` multiplies by 10¹⁰ on the
  way out and divides on the way in (value must divide exactly; gas price floors). `.claude/CLAUDE.md`
  §3 rule 2 and `code-plan.md` §3–5 were corrected to match.
- revm 43: `CfgEnv.disable_balance_check`, `disable_block_gas_limit` and `disable_base_fee` exist
  only behind the cargo features `optional_balance_check`, `optional_block_gas_limit`,
  `optional_no_base_fee` (`revm/Cargo.toml:86-91`). Enabled. `ExecutionResult::gas_used()` is
  deprecated for `tx_gas_used()` (EIP-8037 state-gas split) — the latter is what receipts carry.
- revm 43 `CacheDB::load_account` inserts a fresh entry as `AccountState::NotExisting`, and
  `DbAccount::info()` then returns `None` regardless of the fields (`in_memory_db.rs:189,473`).
  Writing a balance into such an entry is silently invisible to the EVM. `Chain::db_account`
  flips the state to `Touched` first. This cost one failed unit test and one confused smoke run.
- `Database` is not implemented for `&mut CacheDB` in revm 43, so `evm::execute` takes the
  `CacheDB` by `mem::take` and puts it back from `evm.ctx.journaled_state.database`.
- foundry 1.8.1 installed (`~/.foundry/bin`). `cast send` defaults to EIP-1559 with
  `maxPriorityFeePerGas = 1` wei, which floors to 0 tinybar; the effective price is then the base
  fee. `cast send --unlocked --from` drives `eth_sendTransaction`, which works for impersonated
  and predefined senders. `cast mktx` produced the two signed fixtures in `evm::tests`.
- viem 2.56: `getBlockNumber` caches for `cacheTime` (default = polling interval), so a read after
  `evm_revert` must pass `cacheTime: 0`. `simulateContract` surfaces our code-3 revert data as
  `ContractFunctionRevertedError` with `data.errorName`/`data.args` decoded — the custom-error
  path works without any special casing.
- solc-js 0.8.36 compiles `Counter.sol` for `cancun`; fixture checked in at
  `tests/fixtures/Counter.json`.
- Boot, release build, 10 runs on this machine: self-reported "Started in 1 ms"; wall clock from
  spawn to banner 25–27 ms with one 1,078 ms outlier on the first run after linking (macOS
  first-launch check). Binary 5.6 MB.
- Blocky402 `/supported` re-checked 2026-09-07 afternoon: still `hedera:mainnet` only. Discord
  question still open (his action).
- **The repo's hooks only fire when Claude Code is started inside `hanvil/`.** Day 1 ran from
  `~/Desktop/cv`, so `.claude/hooks/pre-commit-gate.sh` never executed and `tests/rpc.rs` went out
  unformatted; CI caught it on `cargo fmt --check` (runs 34104047792, 34104112619). Until the
  session is started from this folder, run `cargo fmt --all && cargo clippy --all-targets -- -D
  warnings` by hand before every commit.

## 16. Day 2 findings (2026-09-07)

- **Correction to §6.** "`/network/exchangerate` fixed 1 ℏ = 12 ¢, matches local-node" was not
  verifiable: no `cent_equivalent`, `hbar_equivalent` or exchange-rate fixture exists anywhere in
  `research/hiero-local-node`, and the consensus-node clone holds only `exchange_rate.proto`
  (`hapi/hedera-protobuf-java-api/src/main/proto/services/exchange_rate.proto:41,48` define the
  fields, not the values). Hanvil serves `hbar_equivalent: 30000, cent_equivalent: 360000`
  (= 12 ¢) and marks it `// VERIFY` in `src/mirror/network.rs`.
- **Mirror list parameters** (`rest/api/v1/openapi.yml:4904,5054,5062,5551`): `limit` defaults to
  25 with range 1..=100; `order` defaults to `asc` except on the account and transaction endpoints
  where the spec uses `orderQueryParamDesc`; `transactions` defaults to true.
- **Mirror 404 and 400 bodies** (`openapi.yml:4521-4600`): `{"_status":{"messages":[{"message":
  "Not found"}]}}` for a missing entity; `Invalid Transaction id. Please use
  "shard.realm.num-sss-nnn" format …` for the SDK's `0.0.x@sss.nnn`. The spec's YAML escapes the
  quotes as `\shard…\`; the quotes are what a real mirror sends.
- **`Block.name`** (`openapi.yml:3371`) is the record file name,
  `2022-05-03T06_46_26.060890949Z.rcd`, derived from the block's consensus timestamp. Hanvil
  computes the civil date with Howard Hinnant's `civil_from_days`; `chrono` stays off the list.
- **AccountInfo required fields** (`openapi.yml:1975-1993`) are all present in Hanvil's response;
  `alias` is null because Hanvil mints EVM-address aliases, not base32 key aliases.
- **`NetworkNode.grpc_proxy_endpoint` deviation.** `openapi.yml:2984-3002` lists it required and
  `openapi.yml:3711` types it as a non-nullable `ServiceEndpoint`. It is HIP-1081's gRPC-web proxy,
  which Hanvil does not run, and no value in the schema means "none". Hanvil sends `null`; the key
  is present, so a reader that checks for the key is satisfied. Marked `// VERIFY` in
  `src/mirror/network.rs` until a running mirror node can be compared against.
- **PR #39's reader** (`research/harness-prs/pr-39.diff`, `src/validation/mirrorNode.ts`) treats
  404 as "not yet" and any other 4xx as a caller error it stops polling on. That is why the
  transaction-id form is a 400 and not an empty list.
