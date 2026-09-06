---
name: pr
description: Draft an upstream PR description (hedera-harness dev, or scaffold-hbar) in the Problem · Change · How to verify · Not covered format required by .claude/CLAUDE.md §10, from the diff of the current branch against its base.
argument-hint: [base-branch, default dev]
---

Base branch: `$ARGUMENTS` or `dev`.

1. `git diff <base>...HEAD --stat` and the full diff. Read every changed file.
2. Write four sections, each ≤ 6 lines, prose not bullets unless listing files:
   **Problem** — what fails or is missing today, with the file:line where it lives.
   **Change** — what the diff does, in the order a reviewer should read it. Nothing the diff
   already says.
   **How to verify** — exact commands and the expected output, including the new test file.
   **Not covered** — what this PR deliberately leaves out and why.
3. For PR 1 (`network: "local"`): state that local mode is for the repair loop and a verdict meant
   for publication still runs against testnet.
   For PR 2 (snapshot per attempt): state the false positive removed — `runChainDeploy` runs every
   attempt, so attempt-N topic messages are visible to the attempt-N+1 validator.
4. Title = the commit subject of the main change. No emoji, no exclamation marks.
5. Append: `🤖 Generated with [Claude Code](https://claude.com/claude-code)` and the session URL
   on their own lines at the end.
6. Check: no line in the description restates the diff; no adjective survives.
