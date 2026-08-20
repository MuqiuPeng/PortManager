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

echo "==> mcp server"
pnpm --dir packages/runtime-mcp exec tsc --noEmit

echo "all clear"
