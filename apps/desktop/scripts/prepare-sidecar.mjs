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

// Every program the bundle carries. The daemon and the CLI speak one protocol
// to each other, so they are built from one tree at one moment and shipped
// together — an update that moved one without the other is how a CLI ends up
// talking to a daemon that has stopped understanding it.
const sidecars = [
  { crate: "runtime-daemon", bin: "runtime-daemon" },
  { crate: "runtime-cli", bin: "runtime" },
];

console.log(`building ${sidecars.map((s) => s.bin).join(", ")} for ${triple}`);
execFileSync(
  "cargo",
  ["build", "--release", ...sidecars.flatMap(({ crate }) => ["-p", crate])],
  { cwd: workspace, stdio: "inherit" },
);

mkdirSync(outDir, { recursive: true });
const suffix = triple.includes("windows") ? ".exe" : "";
for (const { bin } of sidecars) {
  const built = join(workspace, "target", "release", `${bin}${suffix}`);
  const staged = join(outDir, `${bin}-${triple}${suffix}`);
  copyFileSync(built, staged);
  chmodSync(staged, 0o755);
  console.log(`staged ${staged}`);
}
