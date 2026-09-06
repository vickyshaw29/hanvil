#!/usr/bin/env bash
# PostToolUse(Edit|Write). Formats a Rust file the agent just wrote. Never blocks.
INPUT=$(cat)
FILE=$(printf '%s' "$INPUT" | python3 -c 'import sys,json;print(json.load(sys.stdin).get("tool_input",{}).get("file_path",""))' 2>/dev/null)
case "$FILE" in
  *.rs) command -v rustfmt >/dev/null 2>&1 && rustfmt --edition 2024 "$FILE" >/dev/null 2>&1 ;;
esac
exit 0
