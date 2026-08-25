#!/bin/sh
# Check that the project names one home, not three.
#
# The repository URL is written in the crate metadata, in the updater's
# endpoint and in the README, and each is read by something different: the
# first ships inside every binary, the second is where an installed copy looks
# for its next version, and the third is what a person follows. They drifted —
# `Cargo.toml` pointed at a repository that does not exist, for as long as it
# took somebody to audit the release, because nothing reads two of them at once.
set -e

conf="apps/desktop/src-tauri/tauri.conf.json"
for file in Cargo.toml "$conf" README.md; do
  [ -f "$file" ] || { echo "  cannot check: $file is not here (run from the repository root)"; exit 1; }
done

slug() {
  grep -oE 'github\.com/[A-Za-z0-9_-]+/[A-Za-z0-9_.-]+' "$1" |
    sed 's/\.git$//' |
    grep -v '^github\.com/repos' |
    head -1
}

crate=$(slug Cargo.toml)
updater=$(slug "$conf")
readme=$(slug README.md)

if [ -z "$crate" ] || [ -z "$updater" ] || [ -z "$readme" ]; then
  echo "  cannot check: no repository URL found in one of Cargo.toml, $conf, README.md"
  exit 1
fi

if [ "$crate" != "$updater" ] || [ "$crate" != "$readme" ]; then
  echo "  three names for one repository:"
  echo "    Cargo.toml      $crate"
  echo "    tauri.conf.json $updater"
  echo "    README.md       $readme"
  exit 1
fi

echo "  the crate, the updater and the README name one repository"
