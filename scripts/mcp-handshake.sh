#!/bin/sh
# Ask the built MCP server to name its tools, and check the daemon knows the
# requests behind them.
#
# The two sides are written in different languages and neither compiler can see
# the other, so the only thing that catches a drifting name is asking. This is
# cheap: one process, one list, no services touched.
set -e

built="packages/runtime-mcp/dist/index.js"
[ -f "$built" ] || { echo "  not built: $built"; exit 1; }

# The request names the daemon accepts, taken from the protocol itself.
# Both shapes a variant takes: `Named {` with fields, and `Named,` without.
accepted=$(grep -oE '^\s+[A-Z][A-Za-z]+( \{|,)' crates/runtime-ipc/src/protocol.rs |
  tr -d ' {,' |
  sed 's/\([a-z0-9]\)\([A-Z]\)/\1_\2/g' |
  tr 'A-Z' 'a-z' |
  sort -u)

# The requests the MCP server actually sends. `run("name", …)` is how every
# tool reaches the daemon, so the name is the first argument.
sent=$(grep -oE 'run\("[a-z_]+"' "$built" | sed 's/run("//;s/"//' | sort -u)

missing=""
for name in $sent; do
  echo "$accepted" | grep -qx "$name" || missing="$missing $name"
done

if [ -n "$missing" ]; then
  echo "  the MCP server sends requests this daemon does not accept:$missing"
  exit 1
fi
echo "  every request the MCP server sends is one the daemon accepts"
