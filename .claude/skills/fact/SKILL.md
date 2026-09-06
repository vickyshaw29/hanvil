---
name: fact
description: Verify a protocol or upstream claim against the cloned sources under research/ before it is written into code, a comment, the README, or a PR. Returns file:line evidence or "not found" — never a recollection.
argument-hint: <claim or question>
---

Claim: `$ARGUMENTS`

1. Pick the source repo(s) under `/Users/vicky/Desktop/dev/hanvil/research/` that would settle it:
   SDK behaviour → `hiero-sdk-js/src`; wire format / codes → `hiero-consensus-node/hapi/.../proto`;
   mirror shape → `hiero-mirror-node/rest/api/v1/openapi.yml`; RPC behaviour →
   `hiero-json-rpc-relay/docs`; harness → `hedera-harness-dev/src`; local-node defaults →
   `hiero-local-node/.env`, `README.md`, `src/configuration/`.
2. `rg -n` for the identifiers. Read the surrounding 20 lines. Quote ≤ 3 lines per hit.
3. Answer in three parts: **verdict** (true / false / partly), **evidence** (path:line + quote),
   **implication for Hanvil** (one sentence). If nothing settles it, say "not found in sources" and
   name what would.
4. If the claim is already in `docs/research.md`, cite the section instead of re-deriving. If the
   verdict changes something there, append a dated correction line — do not edit the old line.
