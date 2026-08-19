// Build the daemon and stage it as a Tauri sidecar.
//
// The app must be self-contained: launched from Finder it inherits a minimal
// PATH, so a daemon that lives only in `target/` or on the developer's PATH is
// unreachable and the app comes up unable to talk to anything.
//
// Tauri requires sidecars to carry the target triple in their filename and
// strips it when bundling, so `Contents/MacOS/runtime-daemon` ends up next to
// the app binary — exactly where the client looks first.

import { execFileSync } from "node:child_process";
import { copyFileSync, chmodSync, mkdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const workspace = resolve(here, "../../..");
const outDir = join(here, "..", "src-tauri", "binaries");

const triple = execFileSync("rustc", ["-vV"], { encoding: "utf8" })
  .split("\n")
  .find((line) => line.startsWith("host:"))
  ?.replace("host:", "")
  .trim();

if (!triple) {
  throw new Error("could not determine the host target triple from `rustc -vV`");
}

console.log(`building runtime-daemon for ${triple}`);
execFileSync("cargo", ["build", "--release", "-p", "runtime-daemon"], {
  cwd: workspace,
  stdio: "inherit",
});

const built = join(workspace, "target", "release", "runtime-daemon");
const staged = join(outDir, `runtime-daemon-${triple}`);

mkdirSync(outDir, { recursive: true });
copyFileSync(built, staged);
chmodSync(staged, 0o755);
console.log(`staged ${staged}`);
