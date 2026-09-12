# Reference

The complete surfaces, the full recipe schema, and the measurement methods. The
[README](../README.md) links here rather than carrying it inline.

## JSON-RPC, port 7546

Relay shape. Quantities are `0x`-prefixed minimal hex, `null` for a missing optional rather than
an omitted key, and a revert returns `{code: 3, message: "execution reverted…", data: "0x…"}` so
viem and ethers decode custom errors.

```
eth_chainId                     eth_getBlockByNumber            eth_newFilter
eth_blockNumber                 eth_getBlockByHash              eth_newBlockFilter
eth_getBalance                  eth_getBlockReceipts            eth_getFilterChanges
eth_getCode                     eth_getLogs                     eth_getFilterLogs
eth_getStorageAt                eth_getTransactionByHash        eth_uninstallFilter
eth_getTransactionCount         eth_getTransactionReceipt       net_version
eth_gasPrice                    eth_sendRawTransaction          net_listening
eth_maxPriorityFeePerGas        eth_sendTransaction             web3_clientVersion
eth_feeHistory                  eth_call                        web3_sha3
eth_estimateGas
eth_getBlockTransactionCountByHash      eth_getTransactionByBlockHashAndIndex
eth_getBlockTransactionCountByNumber    eth_getTransactionByBlockNumberAndIndex
```

Anvil cheats, and the `hardhat_` aliases for each:

```
evm_snapshot        evm_mine                     anvil_setBalance     anvil_setStorageAt
evm_revert          evm_increaseTime             anvil_setCode        anvil_impersonateAccount
evm_setNextBlockTimestamp                        anvil_setNonce       anvil_stopImpersonatingAccount
anvil_mine          anvil_nodeInfo
```

Plus `hanvil_rejections`, which is hanvil's own — see [the chain
ledger](../README.md#the-chain-ledger).

## Mirror node REST, port 5551

snake_case, timestamps `"sec.nanos"`, ids `0.0.N`, amounts in tinybar, `links.next: null`.

```
/api/v1/accounts/{id|alias|evm}     /api/v1/contracts/{id|address}
/api/v1/accounts/{id}/tokens        /api/v1/contracts/{id}/results
/api/v1/balances                    /api/v1/contracts/results/{hash|txId}
/api/v1/transactions                /api/v1/contracts/results/logs
/api/v1/transactions/{0.0.x-sss-nnn}
/api/v1/topics/{id}                 /api/v1/blocks
/api/v1/topics/{id}/messages        /api/v1/blocks/{number|hash}
/api/v1/topics/{id}/messages/{n}    /api/v1/network/{nodes,fees,exchangerate}
```

`/transactions` applies `account.id`, `transactiontype`, `result`, `timestamp`, `limit` and
`order`. Every other documented filter is refused with `400 Invalid parameter` naming what the
endpoint does apply, rather than answering 200 with an unfiltered list.

## HAPI gRPC, port 50211

Node account is `0.0.3`. `Transaction.signedTransactionBytes` → `SignedTransaction` →
`TransactionBody`; `TransactionResponse{OK}` comes back first and the receipt is available
immediately after.

| Service | Answered |
| --- | --- |
| `CryptoService` | `createAccount` `cryptoTransfer` `cryptoDelete` `cryptoGetBalance` `getAccountInfo` `getTransactionReceipts` `getTxRecordByTxID` |
| `ConsensusService` | `createTopic` `submitMessage` `getTopicInfo` |
| `SmartContractService` | `callEthereum` `contractCallLocalMethod` |
| `NetworkService` | `getVersionInfo` |
| `FileService`, `TokenService`, `ScheduleService`, `FreezeService`, `UtilService`, `AddressBookService` | routed, and answer `NOT_SUPPORTED` |

Those six are registered for one reason: an unregistered service is not routed, and tonic would
answer a call to one with a bare `12 UNIMPLEMENTED`, which the SDK reports as a transport failure
rather than as a refusal the network made. `ContractCreateFlow` deploys through `FileService`, so
it is refused rather than hung.

Paid queries answer `COST_ANSWER` with `cost: 0` and accept `ANSWER_ONLY` with or without a
payment.

## Recipe schema

Schema v3 as `hedera-harness` reads it — the same keys, defaults, error strings, prompts and
artifact layout, ported from `dev` @ `587a2f3` and cited by file and line in `src/harness/`. Five
additions under `chainValidation`, all optional. Upstream's loader ignores unknown keys there, so
a recipe using them still loads on `hedera-harness`:

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

`contract: created` is the newest contract on the chain, the meaning `topic: created` already has,
so a recipe can assert on a deployment whose address it never sees. `rejections` fails on
transactions the node refused.

A phase moves the clock *before* its commands, because `increase_time` shifts the offset and
`block.timestamp` only follows on the next mined block. The flat `deploy` keeps its existing
deploy-then-advance order. Assertion indices run on across the flat block and every phase, so
adding a phase never renumbers a finding id.

Two loader traps: `baseline.commands` must contain a command literally named `install`, and
`validators/*.json` must be `{"commands": []}` and never `{}`.

### Phases, working

From `tests/harness/.harness/spec-phases.yaml`, where a contract reverts with `Deadline: too
early` until its window closes:

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

A week, in the time the sweep took. With `advanceTimeSeconds: 0` the same run fails and the ledger
reads `reverted: Deadline: too early`, which is what makes the pass evidence rather than an
assertion. Both are in `tests/run.rs`.

## A full run, stage by stage

The first run on `examples/hcs-receipts-api`, 2026-09-10, `agent: claude`, `--max-attempts 3`, as
printed:

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

The attempt-2 dump is 43,643 bytes; `hanvil --state <it> --port 0` prints `Started in 0 ms` and its
mirror answers `GET /api/v1/topics/0.0.1033/messages` with the six messages the app wrote.

## Six deviations from the TypeScript harness

Each is recorded in `docs/code-plan.md` §16.

1. The `claude` preset's idle timeout is 600 s, not 90 s — a `Bash` tool call is silent until it
   returns.
2. `CLAUDECODE` and `CLAUDE_CODE_*` are stripped from the agent's environment.
3. A dev server that never prints `Local:` is accepted when `server.url` answers.
4. `@playwright/mcp` is pinned at 0.0.80 and driven over stdio by the harness itself for SMOKE.
5. After a revert the repair prompt gains one sentence saying the chain was reset.
6. The workspace activity log (`logs/workspace-attempt-N.activity.log`, upstream's
   `workspaceWatcher.ts`) is filled by walking the tree every 500 ms rather than by `fs.watch`, so
   a file created and deleted between two walks is missed. The walk runs once more when the agent
   stops, so anything still on disk is recorded.

## Reproducing the numbers

### Boot and memory

Timed inside one process, so no per-iteration `python3` or `curl` startup lands in the figure:

```python
import json, subprocess, time, urllib.request, statistics, signal
def once(port):
    t0 = time.perf_counter()
    p = subprocess.Popen(["./target/release/hanvil", "--silent", "--port", str(port),
                          "--mirror-port", str(port + 100), "--grpc-port", str(port + 200)],
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "eth_chainId", "params": []}).encode()
    req = urllib.request.Request(f"http://127.0.0.1:{port}", data=body,
                                 headers={"content-type": "application/json"})
    while True:
        try:
            if b"result" in urllib.request.urlopen(req, timeout=1).read():
                ms = (time.perf_counter() - t0) * 1000
                break
        except Exception:
            pass
    rss = int(subprocess.run(["ps", "-o", "rss=", "-p", str(p.pid)],
                             capture_output=True, text=True).stdout.strip() or 0)
    p.send_signal(signal.SIGTERM); p.wait()
    return ms, rss / 1024
res = [once(20546 + i * 10) for i in range(5)]
print("median %.0f ms, %.1f MB" % (statistics.median(sorted(r[0] for r in res)),
                                   statistics.median(sorted(r[1] for r in res))))
```

hanvil's column was re-measured 2026-09-12 on an M-series Mac, 10 CPUs, release build — median of
5 for the RPC answer, 11 for the banner, at load average 18 with an agent run in progress. Idle on
2026-09-11 the same binary gave 5 ms and 5.4 MB, so the published figures are the conservative
direction.

### The Docker stack

Measured 2026-09-11 on an otherwise idle machine, Docker given 8 GB, `hiero-local-node` v2.40.2:
median of three, images pulled first and the pull not counted, the stack fully stopped between
runs.

```
git clone https://github.com/hiero-ledger/hiero-local-node && cd hiero-local-node
npm install && npm run build && docker compose pull      # the pull is not timed
node ./build/index.js stop
time node ./build/index.js start
docker stats --no-stream --format '{{.MemUsage}}'
```

### The chain ledger, and a full agent run

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

### One x402 paid request, on each rail

The rail on hanvil, from `examples/toll`, with the payer and facilitator the `hanvil toll` banner
prints:

```
hanvil toll                                    # another terminal
for i in $(seq 1 9); do yarn pay; done | grep -o 'in [0-9.]*s'
```

2026-09-12: 0.167 0.089 0.084 0.061 0.063 0.068 0.090 0.081 0.079 — **median 0.081 s**. The first
call after the rail boots pays for the facilitator's `/supported` fetch and the SDK client, and is
the outlier every time.

The same call against the deployed service, settling on Hedera testnet through Blocky402:

```
set -a; . ~/.hanvil/toll-testnet.env; set +a
for i in 1 2 3 4 5; do TOLL_URL=https://hanvil-toll-production.up.railway.app yarn pay; done
```

2026-09-12: 4.911 5.855 5.829 4.710 4.514 — **median 4.91 s**. Each of those is a real
`CryptoTransfer` on testnet, and each wrote an `x402.receipt.v1` message to topic `0.0.10497255`.

### The TypeScript harness against hanvil

With the [PR #48](https://github.com/hedera-dev/hedera-harness/pull/48) branch built:

```
./target/release/hanvil &
cp -R tests/harness /tmp/project && cd /tmp/project
git init -q -b main . && git add -A && git commit -qm fixture
node <harness>/dist/index.js doctor .harness/spec.yaml
node <harness>/dist/index.js run   .harness/spec.yaml
```
