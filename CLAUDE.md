# hanvil

Engineering standard, stack, and practices: **`.claude/CLAUDE.md`** — read it first.
Then `docs/code-plan.md` (the build) and `docs/research.md` (facts with sources). Upstream repos
are cloned under `research/` (gitignored). `plan.md` is a local working file, also gitignored.

Hanvil: Anvil for Hedera — one Rust binary serving JSON-RPC :7546, mirror REST :5551 and HAPI gRPC
:50211 from one in-memory chain, plus `hanvil run`, a Rust port of `hedera-harness` that drives a
coding agent against that chain, plus two PRs to `hedera-dev/hedera-harness` (`dev` branch) so the
TypeScript harness's CHAIN tier runs locally too. ETHOnline 2026 (project created 2026-09-07: Hanvil / Developer
Tool / 🔨). Submissions due **Sun 2026-09-13 21:30 IST**; check-ins Tue Sep 8 and Fri Sep 11 at
09:29 IST.

Owner: Vicky Prasad — github.com/vickyshaw29.
