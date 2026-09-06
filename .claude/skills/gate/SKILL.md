---
name: gate
description: Run every CI gate locally (fmt, clippy -D warnings, tests, doc, cargo-deny, JS e2e, boot-time budget) and report pass/fail per gate. Use before any push, before claiming a module done, and at every plan.md gate (G1–G3).
disable-model-invocation: true
---

Run each gate from `/Users/vicky/Desktop/dev/hanvil`, in this order, stopping at nothing —
report all of them:

1. `cargo fmt --all -- --check`
2. `cargo clippy --all-targets -- -D warnings`
3. `cargo test --all-targets`
4. `cargo doc --no-deps` (fail on warnings: `RUSTDOCFLAGS="-D warnings"`)
5. `cargo deny check licenses` (skip with a note if cargo-deny is not installed)
6. `node --test tests/js/` against a freshly built binary on random ports (skip with a note if `tests/js` does not exist yet)
7. Boot budget: run `target/release/hanvil --silent --port 0` ten times, measure wall time to the
   "Started in" line, report median; fail if median > 100 ms.

Output one table: gate · result · time · first error line. Then one sentence: ship or not.
Do not fix anything inside this skill; report, then let the user decide.
