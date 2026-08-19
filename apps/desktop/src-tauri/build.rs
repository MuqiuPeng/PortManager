use std::path::PathBuf;

fn main() {
    stage_sidecar();
    tauri_build::build()
}

/// Make sure the daemon sidecar exists before `tauri-build` looks for it.
///
/// `tauri build` stages the real binary through `beforeBuildCommand`, so this
/// normally finds it already there and does nothing. Its job is the other case:
/// a plain `cargo build`, where the sidecar is never used — the daemon sits
/// beside the executable in `target/` — but where a missing file would still
/// fail the build for everyone, including anyone who only wants to compile the
/// workspace.
fn stage_sidecar() {
    let Ok(triple) = std::env::var("TARGET") else {
        return;
    };
    let name = if triple.contains("windows") {
        "runtime-daemon.exe"
    } else {
        "runtime-daemon"
    };

    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let staged = manifest.join("binaries").join(format!(
        "runtime-daemon-{triple}{}",
        if triple.contains("windows") { ".exe" } else { "" }
    ));
    println!("cargo:rerun-if-changed={}", staged.display());

    if staged.is_file() {
        return;
    }
    if let Some(parent) = staged.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    // Prefer a real build if one is lying around, so a `cargo build` bundle is
    // not quietly broken.
    let workspace = manifest.join("..").join("..").join("..");
    for profile in ["release", "debug"] {
        let built = workspace.join("target").join(profile).join(name);
        if built.is_file() && std::fs::copy(&built, &staged).is_ok() {
            return;
        }
    }

    // Nothing to copy. A placeholder keeps `cargo build` working; `tauri build`
    // always overwrites it with the real binary first.
    let _ = std::fs::write(&staged, b"");
    println!(
        "cargo:warning=no runtime-daemon binary to bundle; run `pnpm --dir apps/desktop prepare-sidecar` before `tauri build`"
    );
}
