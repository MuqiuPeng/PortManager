#!/bin/sh
# Check that the MCP server can start where it is shipped, not only where it
# was built.
#
# The app carries the server as a resource: `tauri.conf.json` stages
# `packages/runtime-mcp/dist` into the bundle, and the shim points node at the
# copy inside it. For a year that copy was loose ESM with bare imports —
# `@modelcontextprotocol/sdk`, `zod` — resolved against a `node_modules` tree
# that was never staged with it. Every installed copy failed on the first line:
#
#     Error [ERR_MODULE_NOT_FOUND]: Cannot find package
#     '@modelcontextprotocol/sdk' imported from .../mcp/index.js
#
# Nothing caught it because every check ran the server from the package, where
# `node_modules` happens to sit one directory up. The difference between the two
# locations was the whole bug, and no check could see it.
#
# So this one moves the built server somewhere with no `node_modules` above it
# and asks it to complete a handshake. Cheap: one process, one request.
set -e

built="packages/runtime-mcp/dist"
[ -d "$built" ] || { echo "  not built: $built"; exit 1; }

# Outside the repository, so nothing above it can resolve an import.
staged=$(mktemp -d)
trap 'rm -rf "$staged"' EXIT
cp -R "$built/." "$staged/"

dir="$staged"
while [ "$dir" != "/" ]; do
  if [ -d "$dir/node_modules" ]; then
    echo "  cannot tell: $dir/node_modules is above the copy under test"
    exit 1
  fi
  dir=$(dirname "$dir")
done

# Driven by node rather than a shell pipeline: `head -1` leaves the server
# running with a closed pipe and the check never returns. The server is a
# stdio program, so something has to write a request, read one line and stop.
answer=$(node -e '
  const { spawn } = require("node:child_process");
  const child = spawn(process.execPath, [process.argv[1]]);
  let out = "";
  const done = (text) => { console.log(text.trim().slice(0, 400)); child.kill(); process.exit(0); };
  const timer = setTimeout(() => done(out || "<no answer in ten seconds>"), 10000);
  child.stdout.on("data", (chunk) => {
    out += chunk;
    const line = out.indexOf("\n");
    if (line >= 0) { clearTimeout(timer); done(out.slice(0, line)); }
  });
  child.stderr.on("data", (chunk) => { out += chunk; });
  child.on("error", (err) => { clearTimeout(timer); done(String(err)); });
  child.on("exit", () => { clearTimeout(timer); done(out); });
  child.stdin.end(JSON.stringify({
    jsonrpc: "2.0", id: 1, method: "initialize",
    params: { protocolVersion: "2024-11-05", capabilities: {},
              clientInfo: { name: "check", version: "0" } },
  }) + "\n");
' "$staged/index.js")

# What is under test is whether node can load every module this program
# needs — not whether the machine running the check has a daemon. Those are
# different failures and the messages tell them apart, so this reads the
# message rather than settling for "did it answer".
#
# The first version of this asked only for a handshake, and CI failed on a
# runner with no `runtime` on PATH: the server had loaded perfectly and said
# so in its own words. A check that reports the bug it is not looking for is
# worse than no check, because the next person silences it.
case "$answer" in
  *ERR_MODULE_NOT_FOUND*|*"Cannot find package"*|*"Cannot find module"*|\
  *ERR_UNSUPPORTED_DIR_IMPORT*|*ERR_UNKNOWN_FILE_EXTENSION*)
    echo "  node could not load the shipped server:"
    echo "  $answer"
    echo "  it is staged as loose modules; bundle it instead"
    exit 1
    ;;
  *SyntaxError*)
    echo "  the shipped server is not loadable:"
    echo "  $answer"
    exit 1
    ;;
  *'"instructions"'*)
    echo "  the shipped server starts with nothing beside it, and says how to use it"
    ;;
  *'"serverInfo"'*)
    # It answered, so it loaded — but a handshake without instructions hands a
    # model a list of tools and none of the rules that will refuse half of
    # them. Nothing else notices this going missing: every tool still works,
    # and the agent using them simply does not know that a service cannot be
    # started on its own or that killing somebody else's process is refused.
    echo "  the shipped server answered but sent no instructions"
    echo "  a model would get the tools and none of the rules"
    exit 1
    ;;
  # Its own words, which it can only reach by having loaded.
  *"local-runtime MCP server failed to start"*)
    echo "  the shipped server loaded with nothing beside it"
    echo "  (it got as far as its own error, which is not a packaging fault)"
    ;;
  "")
    echo "  the shipped server said nothing at all, so this cannot tell"
    exit 1
    ;;
  *)
    echo "  unrecognised outcome, so this cannot tell:"
    echo "  $answer"
    exit 1
    ;;
esac
