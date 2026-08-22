#!/bin/sh
# Everything that has to pass before a change is worth deploying.
#
# Collected into one script because the checks were listed in the README and
# not all of them were being run: clippy had never been run at all, on any of
# this. A list somebody has to remember is a list that drifts.
set -e

echo "==> rust tests"
cargo test --workspace

echo "==> clippy"
cargo clippy --workspace --all-targets -- -D warnings

echo "==> windows, without building it"
# `--no-default-features` drops bundled SQLite, whose C build needs a toolchain
# for the target. Everything else still type-checks, tests included — which is
# where platform assumptions hide.
cargo check --workspace --exclude runtime-desktop --all-targets \
  --no-default-features --target x86_64-pc-windows-msvc

echo "==> frontend"
pnpm --dir apps/desktop test

echo "==> the window speaks to this app"
scripts/window-handshake.sh

echo "==> mcp server"
# Built, not merely type-checked. What is registered with an agent is
# `dist/`, and `tsc --noEmit` never writes it — so a rename that landed in
# the source and not in the build passed every check here while every MCP
# call failed against the daemon. The build is the thing that ships.
pnpm --dir packages/runtime-mcp build

echo "==> mcp server speaks to this daemon"
# The names the tools send have to be names the daemon accepts. Nothing else
# in this file would notice them drifting apart: the Rust side type-checks
# its own protocol, and the TypeScript side type-checks its own strings.
scripts/mcp-handshake.sh

echo "all clear"
