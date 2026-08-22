use std::path::PathBuf;

fn main() {
    stage_sidecar();
    stage_resources();
    tauri_build::build()
}

/// Make sure the bundled resources exist before `tauri-build` looks for them.
///
/// The MCP server travels as files rather than as a binary, and those files
/// are a build output — so a clean checkout has none, and `tauri-build` fails
/// on a declared resource path that is not there. That is right for a bundle
/// and wrong for `cargo build`, which makes no bundle and had no business
/// caring: it broke CI, which only type-checks and tests.
///
/// So an empty directory, the same way a missing sidecar gets an empty file. A
/// real build stages the real thing over it first.
fn stage_resources() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dist = manifest
        .join("..")
        .join("..")
        .join("..")
        .join("packages")
        .join("runtime-mcp")
        .join("dist");
    println!("cargo:rerun-if-changed={}", dist.display());
    if dist.is_dir() {
        return;
    }
    let _ = std::fs::create_dir_all(&dist);
    println!(
        "cargo:warning=no MCP server to bundle; run `pnpm --dir packages/runtime-mcp build` before `tauri build`"
    );
}

/// Make sure every sidecar exists before `tauri-build` looks for them.
///
/// `tauri build` stages the real binaries through `beforeBuildCommand`, so this
/// normally finds them already there and does nothing. Its job is the other
/// case: a plain `cargo build`, where the sidecars are never used — the
/// programs sit beside the executable in `target/` — but where a missing file
/// fails the build for everyone, including anyone who only wants to compile the
/// workspace.
///
/// Driven by a list rather than written once per program. It was written for
/// the daemon alone, so adding the CLI as a second sidecar broke every build
/// that had not staged one by hand — including CI, which does not build
/// bundles at all.
fn stage_sidecar() {
    const SIDECARS: [&str; 2] = ["runtime-daemon", "runtime"];
    let Ok(triple) = std::env::var("TARGET") else {
        return;
    };
    let suffix = if triple.contains("windows") { ".exe" } else { "" };

    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest.join("..").join("..").join("..");

    for stem in SIDECARS {
        let staged = manifest
            .join("binaries")
            .join(format!("{stem}-{triple}{suffix}"));
        println!("cargo:rerun-if-changed={}", staged.display());

        if staged.is_file() {
            continue;
        }
        if let Some(parent) = staged.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        // Prefer a real build if one is lying around, so a `cargo build` bundle
        // is not quietly broken.
        let name = format!("{stem}{suffix}");
        let found = ["release", "debug"].into_iter().any(|profile| {
            let built = workspace.join("target").join(profile).join(&name);
            built.is_file() && std::fs::copy(&built, &staged).is_ok()
        });
        if found {
            continue;
        }

        // Nothing to copy. A placeholder keeps `cargo build` working; `tauri
        // build` always overwrites it with the real binary first.
        let _ = std::fs::write(&staged, b"");
        println!(
            "cargo:warning=no {stem} binary to bundle; run `pnpm --dir apps/desktop prepare-sidecar` before `tauri build`"
        );
    }
}
