#!/usr/bin/env bash
# Drop the bus protocol into a project so every agent working there reads it,
# whichever CLI they run under (CLAUDE.md for Claude Code, AGENTS.md for the rest).
#
#   scripts/init-workdir.sh /path/to/project
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SNIPPET="$ROOT/protocol/PROTOCOL.md"
TARGET="${1:-$PWD}"

[[ -d "$TARGET" ]] || { echo "not a directory: $TARGET" >&2; exit 1; }

for f in AGENTS.md CLAUDE.md; do
  path="$TARGET/$f"
  if [[ -f "$path" ]] && grep -q "agent-bus:begin" "$path"; then
    # Replace the existing block rather than stacking duplicates.
    node -e '
      const fs = require("fs");
      const [path, snippet] = process.argv.slice(1);
      const body = fs.readFileSync(path, "utf8");
      const block = fs.readFileSync(snippet, "utf8").trim();
      fs.writeFileSync(path, body.replace(
        /<!-- agent-bus:begin -->[\s\S]*?<!-- agent-bus:end -->/,
        block,
      ));
    ' "$path" "$SNIPPET"
    echo "updated  $path"
  else
    [[ -f "$path" ]] && printf '\n' >> "$path"
    cat "$SNIPPET" >> "$path"
    echo "appended $path"
  fi
done
