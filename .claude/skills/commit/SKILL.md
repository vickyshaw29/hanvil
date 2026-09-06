---
name: commit
description: Stage and commit the current work as one or more conventional, scoped, imperative commits that satisfy .claude/CLAUDE.md §9 and pass the pre-commit gate. Use after a green test or every two hours of work.
---

1. `git status --porcelain` and `git diff --stat`. If the change spans more than one concern
   (e.g. `rpc` and `mirror`), split into separate commits by path — one concern per commit.
2. Refuse to stage `research/`, `plan.md`, `private/`, `.env*`, keys, `chain-signer.json`.
3. For each commit: subject `type(scope): imperative, lowercase, ≤72 chars`; types
   feat|fix|test|docs|refactor|chore|perf|build|ci; scopes match module names (`rpc`, `mirror`,
   `hapi`, `evm`, `state`, `keys`, `cli`, `proto`, `tests`, `readme`, `ci`).
   Body only when the diff does not explain itself: what was wrong, what changed, how verified.
4. End every message with the trailers:
   `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>` and the `Claude-Session:` line
   from the session.
5. Commit with `git commit -m` (subject) `-m` (body). The pre-commit hook runs fmt, clippy and the
   forbidden-path guard; if it blocks, fix the cause — never bypass with `--no-verify`.
6. Print `git log --oneline -n <count>` for the commits made.
