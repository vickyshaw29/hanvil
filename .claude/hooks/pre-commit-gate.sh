#!/usr/bin/env bash
# PreToolUse(Bash). Enforces .claude/CLAUDE.md §4 and §9 at `git commit` time.
# Exit 2 blocks the command and shows stderr to the agent. Any other exit lets it through.
set -u
INPUT=$(cat)
CMD=$(printf '%s' "$INPUT" | python3 -c 'import sys,json;print(json.load(sys.stdin).get("tool_input",{}).get("command",""))' 2>/dev/null || true)
case "$CMD" in *"git commit"*) ;; *) exit 0;; esac

REPO=/Users/vicky/Desktop/dev/hanvil
cd "$REPO" || exit 0
git rev-parse --is-inside-work-tree >/dev/null 2>&1 || exit 0
fail() { printf 'pre-commit-gate: %s\n' "$*" >&2; exit 2; }

# 1. Never commit these paths (CLAUDE.md §9).
STAGED=$(git diff --cached --name-only)
BAD=$(printf '%s\n' "$STAGED" | grep -E '^(research/|plan\.md$|private/|\.env|.*\.pem$|.*\.key$|.*chain-signer\.json$)' || true)
[ -n "$BAD" ] && fail "refusing to commit forbidden paths:
$BAD"

# 2. Conventional commit subject (CLAUDE.md §9). Checks the -m argument if present.
MSG=$(printf '%s' "$CMD" | python3 -c '
import sys,re,shlex
cmd=sys.stdin.read()
try: parts=shlex.split(cmd)
except Exception: parts=[]
for i,p in enumerate(parts):
    if p=="-m" and i+1<len(parts): print(parts[i+1].splitlines()[0]); break
    if p.startswith("-m") and len(p)>2: print(p[2:].splitlines()[0]); break
')
if [ -n "$MSG" ]; then
  printf '%s' "$MSG" | grep -qE '^(feat|fix|test|docs|refactor|chore|perf|build|ci)(\([a-z0-9/_-]+\))?!?: [a-z0-9`]' \
    || fail "subject must be conventional and imperative, e.g. 'feat(rpc): eth_getLogs over in-memory receipts' — got: $MSG"
  [ ${#MSG} -le 72 ] || fail "subject is ${#MSG} chars; keep it ≤ 72"
fi

# 3. No unwrap/expect added under src/ (CLAUDE.md §5). Escape hatch: trailing comment `// gate: allow`.
ADDED=$(git diff --cached -U0 -- 'src/**/*.rs' 'src/*.rs' 2>/dev/null | grep -E '^\+' | grep -vE '^\+\+\+' || true)
VIOL=$(printf '%s\n' "$ADDED" | grep -E '\.(unwrap|expect)\(' | grep -v 'gate: allow' || true)
[ -n "$VIOL" ] && fail "unwrap()/expect() added under src/ (allowed only in tests). Lines:
$VIOL"

# 4. Rust gates when a Cargo project exists.
if [ -f Cargo.toml ]; then
  cargo fmt --all -- --check >/dev/null 2>&1 || fail "cargo fmt --check failed; run cargo fmt"
  OUT=$(cargo clippy --all-targets --quiet -- -D warnings 2>&1) || fail "clippy failed:
$(printf '%s' "$OUT" | tail -40)"
fi
exit 0
