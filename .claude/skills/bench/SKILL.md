---
name: bench
description: Produce the measured numbers for the README and PR 1 (boot time, eth_call latency, snapshot/revert time, memory) from the release binary, and the same for hiero-local-node when it is running. Every number in the README must come from this skill.
disable-model-invocation: true
---

From `/Users/vicky/Desktop/dev/hanvil`, `cargo build --release` first.

1. Boot: 10 runs of `target/release/hanvil --silent --port 0`, wall time to the "Started in" line.
   Report min / median / max in ms.
2. RSS: `ps -o rss=` of a running instance after boot, in MB.
3. `eth_call` latency: 200 sequential `eth_chainId` and 200 `eth_call` to a deployed counter;
   report p50 / p95 in ms.
4. Snapshot/revert: with 1,000 accounts and 100 contracts, time `evm_snapshot` and `evm_revert`.
5. If `docker ps` shows hiero-local-node: time `npm run start` to the "successfully started" line
   and its total container RSS. If not running, write "not measured" — never estimate.
6. Print a markdown table with the exact commands used in a footnote. Date-stamp it.
