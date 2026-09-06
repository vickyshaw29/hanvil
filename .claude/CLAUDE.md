# Hanvil — engineering standard

This file is the contract for every line of code, commit, PR, and sentence produced in this repo.
Read order for a fresh session: this file → `docs/code-plan.md` (the build) →
`docs/research.md` (every fact, with file:line) → `plan.md` (local, gitignored: dates and gates). Upstream sources are cloned under
`research/` (gitignored); when a fact is needed, grep the source, do not recall it.

Working directory is `/Users/vicky/Desktop/dev/hanvil`. Use absolute paths.

---

## 1. What we are building, in one breath

`hanvil`: one Rust binary, one in-memory chain, three listeners — JSON-RPC :7546 (relay shape),
mirror REST :5551, HAPI gRPC :50211 — so `@hiero-ledger/sdk`, viem/wagmi, hardhat, foundry and
`hedera-harness` connect without knowing it is not hiero-local-node. Anvil ergonomics on top:
predefined accounts, `evm_snapshot`/`evm_revert`, time travel, impersonation, sub-100 ms boot.
Two PRs to `hedera-dev/hedera-harness` (`dev` branch) make the harness run on it.
Deadline Sun 2026-09-13 21:30 IST.

## 2. Stack — exact, pinned, no substitutions without a note in `docs/code-plan.md`

| Layer | Crate / tool | Version (crates.io 2026-09-07) | Notes |
| --- | --- | --- | --- |
| Toolchain | Rust stable, edition 2024 | `rust-toolchain.toml` pins the channel | `cargo fmt`, `cargo clippy -D warnings` are gates |
| EVM | `revm` | 43.0.0 | `Context::mainnet().with_db(CacheDB<EmptyDB>)`, `build_mainnet()`, `transact_commit`; features `std`, `serde` |
| Primitives | `alloy-primitives` | **match revm 43's pin** (check `cargo tree`) | Address, U256, B256, keccak256 |
| Tx decoding | `alloy-consensus`, `alloy-rlp`, `alloy-eips` | 2.4.1 / 0.3.16 / matching | legacy, 2930, 1559 envelopes; sender recovery |
| gRPC | `tonic` + `tonic-prost-build` | 0.14.6 | h2c plaintext; codegen in `build.rs` |
| Protobuf | `prost`, `prost-build`, `protoc-bin-vendored` | 0.14.4 | 119-file HAPI closure vendored in `proto/services/` |
| HTTP | `axum` | 0.8.9 | JSON-RPC and mirror REST on separate listeners |
| Runtime | `tokio` | 1.53.1 | `rt-multi-thread`, `macros`, `signal`, `net` |
| Serde | `serde`, `serde_json` | latest 1.x | `#[serde(rename_all = "snake_case")]` for mirror, hex quantities for RPC |
| Crypto | `k256` 0.14 (`ecdsa`), `ed25519-dalek` 2.x, `sha2` (SHA-384 running hash), `sha3`/alloy keccak | | signature verification of HAPI bodies |
| CLI | `clap` | 4.6.6 | derive; `--help` reads like Anvil's |
| Errors | `thiserror` (lib), `anyhow` (bin `main.rs` only) | | |
| Logs | `tracing`, `tracing-subscriber` | | one line per tx; `--silent` |
| Lock | `parking_lot::RwLock` | | sync lock, never held across `.await` |
| JS tests | `@hiero-ledger/sdk` ^2.86, `viem` ^2, Node 20 | | `tests/js/`, run against a built binary |
| Harness | `hedera-harness@next` (2.0.0-rc.4, schemaVersion 3) | | PRs against `dev` |

Deny list: no `ethers-rs`, no `web3`, no `actix`, no `reqwest` in the binary (Hanvil makes no
outbound calls), no `unsafe`, no `lazy_static` (use `std::sync::LazyLock`), no `chrono` (use
`std::time` + a `Clock` trait).

## 3. Architecture rules (non-negotiable)

1. **One `Chain` struct behind one `RwLock`.** Every request takes it once. No second source of
   truth. No `Arc<Mutex<..>>` per submodule.
2. **Balances are authoritative in tinybar** on `Account`. The revm `CacheDB` balance is derived
   (`× 10¹⁰`) before execution and written back after. Never read a balance from `CacheDB`.
3. **Every id is a newtype.** `EntityId(u64)`, `Tinybar(u64)`, `Weibar(U256)`, `TxId`, `Timestamp`.
   Conversions exist only in `evm/units.rs`. A raw `u64` crossing a module boundary is a bug.
4. **Protocol shapes are copied, never guessed.** Each response struct carries a comment with
   the spec path and line (`openapi.yml:…`, `openrpc.json`, `transaction_receipt.proto:43`).
   If the spec is ambiguous, match what `hiero-local-node` returns; if unknown, mark `// VERIFY`.
5. **Error codes map 1:1 to upstream.** HAPI rejections use `ResponseCodeEnum` values
   (`INVALID_SIGNATURE 7`, `DUPLICATE_TRANSACTION 11`, `SUCCESS 22`, …). JSON-RPC uses the
   relay's codes (`-32601` unsupported, `-32000` server, `3` execution reverted with `data`).
   Mirror 404 uses `{"_status":{"messages":[{"message":"Not found"}]}}`.
6. **Fail loud, never fall back silently.** Unsupported transaction body → `NOT_SUPPORTED` with a
   message naming the alternative. Unknown RPC method → `-32601` with the method name. HTS call →
   revert with `hanvil: HTS system contract not emulated`. No `Ok(default)` on an unknown input.
7. **Time is injected.** A `Clock` trait, wall-clock in the binary, fixed in tests.
   `evm_increaseTime` / `evm_setNextBlockTimestamp` mutate `Chain`, not the clock.
8. **Snapshot = clone.** `Chain: Clone`. `evm_revert(id)` swaps and truncates later snapshots.
   Anything added to `Chain` must be `Clone` + `Serialize` or it does not go in `Chain`.
9. **No outbound network calls.** Hanvil is hermetic. The binary never fetches anything.
10. **Bind `127.0.0.1` by default.** `--host 0.0.0.0` is explicit.

## 4. Code quality gates (CI fails otherwise)

- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, `cargo doc
  --no-deps` warning-free, `cargo deny check licenses` (MIT/Apache-2.0/BSD only).
- `node tests/js` green against the built binary.
- Boot time asserted in CI: `hanvil --silent` must print its banner within **100 ms** (measured
  by the test, not claimed).
- Coverage is not a gate; a test per behaviour is. Every bug fix lands with the test that would
  have caught it, in the same commit, test first.

## 5. Rust practice

- `#![forbid(unsafe_code)]` in `main.rs`. `#![warn(missing_docs)]` on public items in `state`,
  `evm`, `hapi`.
- No `unwrap()`/`expect()` outside `#[cfg(test)]` and `build.rs`. Use `?` with typed errors:
  `hapi::Error`, `rpc::Error`, `mirror::Error`, each `thiserror`, each with an
  `into_response()` that produces the exact upstream wire error.
- Functions do one thing and fit on a screen. A handler parses → validates → mutates `Chain` →
  renders; the mutation lives in `state/` as a method on `Chain`, unit-tested without any server.
- `pub(crate)` by default; `pub` only for the CLI-facing API.
- Comments explain *why* and cite the spec; never restate the code. A comment that says what the
  next line does is deleted.
- Naming follows the domain: `payer`, `consensus_timestamp`, `running_hash`, `long_zero_address`,
  `alias`. Never `data`, `info`, `helper`, `utils` as names.
- No `async` inside `state/` or `evm/` — pure, synchronous, testable. Async stops at the handlers.
- Allocation is not a concern; clarity is. Do not optimise until a measurement says so.

## 6. Protocol fidelity practice

- **JSON-RPC**: quantities are `0x`-prefixed minimal hex; `null` for missing optionals, not
  omitted; block objects carry `baseFeePerGas`; revert returns `{code:3, message:"execution
  reverted…", data:"0x…"}` so viem/ethers decode custom errors. Unsupported methods return the
  relay's exact `-32601` set (`docs/research.md` §5).
- **Mirror**: snake_case, timestamps `"sec.nanos"`, ids `0.0.N`, amounts tinybar, `links.next:
  null`, transaction ids `0.0.x-sss-nnn` in URLs. Required-field sets from `openapi.yml`
  (`docs/research.md` §6) are complete or the response is wrong.
- **HAPI**: `Transaction.signedTransactionBytes` → `SignedTransaction` → `TransactionBody`.
  Precheck order and codes are fixed in `docs/code-plan.md` §5. `TransactionResponse{OK}` first,
  receipt available immediately after. Node account is `0.0.3`. Paid queries answer
  `COST_ANSWER` with `cost: 0` and accept `ANSWER_ONLY` with or without a payment.
- **Signatures**: ECDSA secp256k1 over `keccak256(bodyBytes)`, 64-byte r‖s; ED25519 over
  `bodyBytes`. Fixtures are produced by the real SDK and checked in under `tests/fixtures/`.
- **Units**: 1 tinybar = 10¹⁰ wei. Reject `tx.value` not divisible by 10¹⁰ with the relay's error.
- **Identity**: long-zero address = shard(4) ‖ realm(8) ‖ num(8); alias = keccak(pubkey)[12..].
  Predefined accounts and keys are byte-identical to hiero-local-node's
  `accountConfiguration.json`; ids 1002–1031; 10,000 ℏ each.

## 7. Testing practice

- **Unit** (`src/**`): units, long-zero, sig verify vectors, running hash v3, snapshot/revert,
  id allocation, precheck ordering. Table-driven where there are ≥ 3 cases.
- **Integration** (`tests/*.rs`): boot on random ports; JSON-RPC deploy → call → logs; tonic client
  create → transfer → balance → delete → receipt; mirror shape goldens (`tests/golden/*.json`,
  diffed byte-for-byte after key sorting).
- **E2E** (`tests/js/`): the exact harness sequence with `@hiero-ledger/sdk`
  (`Client.forLocalNode()`, `setECDSAKeyWithAlias`, `AccountBalanceQuery`, `TransferTransaction`,
  `AccountDeleteTransaction`); viem deploy + read logs; `GET /accounts/{evmAlias}` resolves.
- **Harness** (`.github/workflows/ci.yml`, last job): `hedera-harness@next` Tier 3.5 on a trivial
  recipe with `network: local`, no secrets. This job is the demo link on the submission form.
- Tests never sleep on wall time; they poll with a deadline. Tests never depend on port 7546
  being free; they take `--port 0` and read the bound port from the banner.

## 8. Logging and UX

- Banner on boot (`docs/code-plan.md` §11): endpoints, chain id, accounts table, "Started in N ms".
- One line per transaction at `info`: kind, id/hash, from, to, gas, result, block. Anvil style.
- Every rejection tells the caller what was wrong and what would have been accepted:
  `INVALID_TRANSACTION_START: validStart 2026-09-08T10:00:00Z is 240s in the past; window is
  -180s..+60s`.
- `--silent` prints nothing. `RUST_LOG` overrides. No colour when not a TTY.

## 9. Git

- Repo `vickyshaw29/hanvil`, MIT, public from the first commit. `research/`, `target/`,
  `node_modules/`, `.harness/` are gitignored. **Never commit `research/`.**
- Conventional commits, imperative, scoped, one change each:
  `feat(rpc): eth_getLogs over in-memory receipts`, `fix(hapi): accept COST_ANSWER without payment`,
  `test(mirror): golden for /accounts/{evm}`, `docs(readme): measured boot time`.
- Commit at least every two hours of work and at every green test. ETHGlobal reviews commit
  history manually; a single 3,000-line commit is disqualifying in spirit.
- `main` always builds and passes `cargo test`. Work on short branches, fast-forward merge.
- Trailer on every commit:
  `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>` and the `Claude-Session:` line.

## 10. Upstream PR practice (`hedera-harness` on `dev`, scaffold-hbar if time)

- Read PRs #39 and #15 before touching `chainSigner.ts`. Rebase on `dev` every morning.
- One concern per PR. Match their TypeScript style exactly: no new dependencies, `node --test`
  tests in `test/*.test.mjs`, `npm run typecheck` clean, CHANGELOG entry under `Unreleased`.
- Description = **Problem · Change · How to verify · Not covered**. Nothing the diff already says.
  PR 1 states that local is the inner loop and the published verdict still runs on testnet.
  PR 2 states the false positive it removes (attempt-1 topic messages read by the attempt-2
  validator).
- Never rename, reformat, or "clean up" lines the PR does not need. Reviewers read diffs.

## 11. Documentation practice

- README order: one-line what → measured numbers table → 3-command quickstart → endpoint table →
  what is emulated / what is not (one flat list) → harness integration → architecture diagram →
  licence and NOTICE. No adjectives. Every number in the README was produced by a command that is
  also in the README.
- Every stub is declared in "not emulated". A stub a judge finds costs more than ten declared.
- `docs/research.md` is append-only with dates; corrections are new lines, not edits.

## 12. Definition of done

A module is done when: unit tests pass, its integration test passes, clippy is clean, its public
items are documented, its error paths return upstream codes, and its behaviour is listed in the
README (emulated or not).

The project is done when: the CI harness job is green with no secrets, the README numbers were
measured today, both PRs are open against `dev` with tests, the video is 2–4 min at ≥ 720p with
voice, and the ETHGlobal form is submitted before 18:00 IST on Sep 13.

## 13. Voice — applies to code comments, commits, README, PRs, video, and replies

First sentence is a fact or a number. No adjectives, no hedging, no exclamation marks, no emojis
in technical text. Say what was rejected and why. Short sentences. Prose over bullet-salad in
descriptions; tables for comparisons. Replies to Vicky: conclusion first, detail on request.

## 14. Anti-patterns that get reverted on sight

Big-bang commits · numbers not produced by a command · `unwrap()` in library code · a fallback
that hides an unsupported input · a response field invented rather than copied from the spec ·
a `TODO` without a matching entry in `plan.md` · a test that sleeps · holding the lock across an
`.await` · an outbound HTTP call from the binary · a PR that touches lines it does not need ·
a README claim that was not run today.

## 15. Repo automation (this folder)

| Path | Role |
| --- | --- |
| `.claude/settings.json` | Permission allowlist (cargo, git, node, local curl, read-only tools); denies force-push, hard reset, sudo, reading signer keys. Registers the two hooks. |
| `.claude/hooks/pre-commit-gate.sh` | `PreToolUse(Bash)` on `git commit`: blocks forbidden paths (`research/`, `plan.md`, `private/`, keys), non-conventional or >72-char subjects, `unwrap`/`expect` added under `src/` (escape: `// gate: allow`), then `cargo fmt --check` and `clippy -D warnings`. Tests run in `/gate` and CI, not here. |
| `.claude/hooks/fmt-on-write.sh` | `PostToolUse(Edit\|Write)`: `rustfmt` on any `.rs` just written. |
| `/gate` | Every CI gate locally, one table, ship/no-ship. Run at G1–G3 and before push. |
| `/commit` | Splits by concern, conventional subjects, trailers, never `--no-verify`. |
| `/pr [base]` | Problem · Change · How to verify · Not covered, from the diff against `dev`. |
| `/day [done <fragment>]` | Today's `plan.md` checklist, next gate, hours to deadline; flips a checkbox. |
| `/bench` | Produces every number the README is allowed to print. |
| `/fact <claim>` | Settles a claim from `research/` with `path:line`, or says "not found". |
| `agents/spec-checker` | Field-by-field diff of a Hanvil type against openapi / proto / openrpc. |
| `agents/standard-reviewer` | Ranked findings against §3 §5 §6 §7 §9 for a diff. |
| `agents/upstream-reader` | "How does the SDK / harness / relay actually do X" with evidence. |

The hooks are the floor; `/gate` is the bar; CI is the proof. A commit that needs `--no-verify`
is a commit that is wrong.
