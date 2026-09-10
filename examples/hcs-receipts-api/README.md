# hcs-receipts-api — the dogfood recipe

`hanvil run` on this directory drives a coding agent to build a receipts API on a Hedera
Consensus Service topic, against the local network hanvil runs in the same process. The brief
is `.harness/prd.md`; the recipe is `.harness/spec.yaml`. Everything the agent produces lands
on a `harness/run-*` branch; this directory ships only the brief, the recipe and `package.json`.

The recipe runs every stage: `npm install` and `node --check` under ASSERT, `node scripts/seed.js`
plus three chain assertions under CHAIN, two routes through a headless browser under SMOKE, and
three `eval.json` checks by a validator agent under EVALUATE. The first recorded run
(2026-09-10, `claude`, 9 min 32 s) failed attempt 1 on SMOKE — the page had no favicon and the
browser logged the 404 as a console error — reverted the chain, and passed attempt 2 with the
finding fixed; the numbers are in the top-level README.

```
cp -R examples/hcs-receipts-api /tmp/receipts && cd /tmp/receipts
git init -b main && git add -A && git commit -m "seed"
hanvil doctor
hanvil run
```
