#!/bin/sh
# Check that every command the window invokes is one the app registers.
#
# The window reaches Rust through `invoke("name")` — a string on one side, a
# function name on the other, and no compiler sees both. Renaming the commands
# left five of them pointing at names that no longer existed, and because every
# caller has a `catch`, the window showed an empty list instead of an error.
# Nothing else in this repository would have noticed.
set -e

api="apps/desktop/src/api.ts"
lib="apps/desktop/src-tauri/src/lib.rs"

called=$(grep -oE 'invoke(<[^>]*>)?\("[a-z_]+"' "$api" |
  sed 's/.*("//;s/"//' | sort -u)
registered=$(sed -n '/generate_handler!\[/,/\]/p' "$lib" |
  grep -oE 'commands::[a-z_]+' | sed 's/commands:://' | sort -u)

missing=""
for name in $called; do
  echo "$registered" | grep -qx "$name" || missing="$missing $name"
done

if [ -n "$missing" ]; then
  echo "  the window invokes commands the app does not register:$missing"
  exit 1
fi
echo "  every command the window invokes is one the app registers"
