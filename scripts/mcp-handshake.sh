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

# And the replies. A request name that matches is only half of it: the server
# also decides what to do by the `type` on the answer, and a rename left one of
# those pointing at a tag the daemon stopped sending — so the call succeeded
# and the tool reported that it had got something unexpected.
emitted=$(sed -n '/pub enum ResponseBody/,/^}/p' crates/runtime-ipc/src/protocol.rs |
  grep -oE '^\s{4}[A-Z][A-Za-z]+' | tr -d ' ' |
  sed 's/\([a-z0-9]\)\([A-Z]\)/\1_\2/g' | tr 'A-Z' 'a-z' | sort -u)
expected=$(grep -oE 'body\.type === "[a-z_]+"' "$built" | sed 's/.*"\(.*\)"/\1/' | sort -u)

stale=""
for name in $expected; do
  echo "$emitted" | grep -qx "$name" || stale="$stale $name"
done

if [ -n "$stale" ]; then
  echo "  the MCP server waits for replies the daemon never sends:$stale"
  exit 1
fi

echo "  every request the MCP server sends is one the daemon accepts, and every"
echo "  reply it waits for is one the daemon sends"
