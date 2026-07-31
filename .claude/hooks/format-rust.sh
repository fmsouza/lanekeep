#!/usr/bin/env bash
#
# Format a Rust file immediately after it is written or edited.
#
# The point is not tidiness — it is that `just check` starts with `cargo fmt --check`,
# so an unformatted file fails the gate before clippy or the tests get a chance to say
# anything more useful. Formatting on write means the first failure you see is a real one.
#
# Invoked as a PostToolUse hook. Reads the tool payload as JSON on stdin.

set -uo pipefail

payload="$(cat)"

file_path="$(printf '%s' "$payload" | python3 -c '
import json, sys
try:
    data = json.load(sys.stdin)
except (json.JSONDecodeError, ValueError):
    sys.exit(0)
print(data.get("tool_input", {}).get("file_path", ""))
' 2>/dev/null)"

[ -n "$file_path" ] || exit 0
[ -f "$file_path" ] || exit 0

case "$file_path" in
    *.rs) ;;
    *) exit 0 ;;
esac

# Failure here means the file does not parse, which the edit itself will surface far
# more clearly than a hook can. Stay quiet and let the real error through.
rustfmt --edition 2024 "$file_path" 2>/dev/null || true

exit 0
