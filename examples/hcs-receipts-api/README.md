# hcs-receipts-api — the dogfood recipe

`hanvil run` on this directory drives a coding agent to build a receipts API on a Hedera
Consensus Service topic, against the local network hanvil runs in the same process. The brief
is `.harness/prd.md`; the recipe is `.harness/spec.yaml`. Everything the agent produces lands
on a `harness/run-*` branch; this directory ships only the brief, the recipe and `package.json`.

The chain assertion asks for three messages on the topic where the brief asks for one, so the
first attempt fails on the chain alone and the second is a repair. That is the loop, shown on
purpose.

```
cp -R examples/hcs-receipts-api /tmp/receipts && cd /tmp/receipts
git init -b main && git add -A && git commit -m "seed"
hanvil doctor
hanvil run
```
