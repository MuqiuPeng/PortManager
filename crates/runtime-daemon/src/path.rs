//! Giving the daemon the `PATH` a terminal would have had.
//!
//! A process launched from Finder inherits `/usr/bin:/bin:/usr/sbin:/sbin` and
//! nothing else — not Homebrew, not a node version manager, not Docker. The
//! daemon spawns every service through a shell that inherits its environment,
//! so from inside the app not one project could start:
//!
//!     sh: pnpm: command not found        (exit 127)
//!
//! This was invisible for as long as it was, because during development the
//! daemon is always started by the CLI from a terminal, where the `PATH` is
//! already right. The packaged app was the first thing to launch it any other
//! way, and it could not start a single service.
//!
//! The runtime already noticed — `doctor` reported nine services whose command
//! "is not on this daemon's PATH" — but noticing is not fixing, and a warning
//! that every project trips is one nobody can act on.
//!
//! So the login shell is asked what it would have given us, and its answer is
//! appended to what we inherited. Appended, not substituted: an environment
//! somebody set deliberately keeps precedence, and this only adds places to
//! look.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::time::Duration;

/// How long the login shell gets. A profile that takes longer than this is one
/// that would make every launch feel broken, and a short `PATH` is better than
/// a daemon that never finishes starting.
const PATIENCE: Duration = Duration::from_secs(3);

/// Ask the shell where it would look, and add whatever we were missing.
pub async fn widen() {
    let inherited = std::env::var_os("PATH").unwrap_or_default();

    // Both, because neither is enough on its own. A login shell reads the
    // profile (`.zprofile`, `.bash_profile`) and an interactive one reads the
    // rc file (`.zshrc`, `.bashrc`) — and a version manager is conventionally
    // installed in the second. Measured on this machine: the login shell
    // offered seventeen directories and no nvm, the interactive one eleven
    // directories including it. Asking for both at once is not a shortcut:
    // `zsh -i -l -c` answered with nothing at all.
    let (login, interactive) = tokio::join!(ask(&["-l"]), ask(&["-i"]));

    let mut merged = inherited.clone();
    let mut added = 0;
    for answer in [login, interactive].into_iter().flatten() {
        let (next, gained) = merge(&merged, &answer);
        merged = next;
        added += gained;
    }
    if added == 0 {
        return;
    }
    // Logged rather than silent: a daemon that quietly rewrites its own
    // environment is one nobody can explain the behaviour of later.
    tracing::info!(added, "widened PATH from the shell");
    std::env::set_var("PATH", merged);
}

/// What the shell prints before the answer, so a talkative profile does not
/// become part of it.
///
/// An interactive shell runs the rc file, and rc files greet people, print
/// tips, and warn about updates. Taking the whole of stdout would fold that
/// into `PATH`.
const MARKER: &str = "__runtime_path__";

/// The user's shell, run with `flags`, asked for its `PATH`.
#[cfg(unix)]
async fn ask(flags: &[&str]) -> Option<String> {
    let shell = std::env::var_os("SHELL").unwrap_or_else(|| OsString::from("/bin/sh"));
    let script = format!("printf '\\n%s%s' '{MARKER}' \"$PATH\"");

    let mut command = tokio::process::Command::new(&shell);
    command
        .args(flags)
        .arg("-c")
        .arg(&script)
        // A shell that wants a terminal must not get one, or it can sit
        // waiting for input that will never arrive.
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    let out = match tokio::time::timeout(PATIENCE, command.output()).await {
        Ok(Ok(out)) => out,
        Ok(Err(err)) => {
            tracing::warn!(%err, ?flags, "could not run the shell");
            return None;
        }
        Err(_) => {
            tracing::warn!(?flags, "the shell did not answer in time");
            return None;
        }
    };

    let text = String::from_utf8_lossy(&out.stdout);
    let answer = text.rsplit_once(MARKER).map(|(_, path)| path.trim().to_string());
    match answer {
        Some(path) if !path.is_empty() => Some(path),
        _ => {
            tracing::warn!(?flags, status = ?out.status, "the shell reported no PATH");
            None
        }
    }
}

/// Windows hands a GUI process the user's full `PATH` already.
#[cfg(not(unix))]
async fn ask(_flags: &[&str]) -> Option<String> {
    None
}

/// Everything inherited, in order, then everything discovered that is new.
///
/// Returns how many were added so the caller can stay quiet when there is
/// nothing to say.
fn merge(inherited: &OsStr, discovered: &str) -> (OsString, usize) {
    let mut seen: HashSet<OsString> = HashSet::new();
    let mut kept: Vec<OsString> = Vec::new();

    for entry in std::env::split_paths(inherited) {
        let entry = entry.into_os_string();
        if !entry.is_empty() && seen.insert(entry.clone()) {
            kept.push(entry);
        }
    }
    let before = kept.len();

    for entry in std::env::split_paths(discovered.trim()) {
        let entry = entry.into_os_string();
        if !entry.is_empty() && seen.insert(entry.clone()) {
            kept.push(entry);
        }
    }

    let joined = std::env::join_paths(kept.iter()).unwrap_or_else(|_| inherited.to_os_string());
    (joined, kept.len() - before)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(v: &OsStr) -> Vec<String> {
        std::env::split_paths(v)
            .map(|p| p.to_string_lossy().into_owned())
            .collect()
    }

    /// What the app suffers: a minimal PATH gains the places tools actually live.
    #[test]
    fn a_finder_launch_gains_the_places_tools_live() {
        let (merged, added) = merge(
            OsStr::new("/usr/bin:/bin"),
            "/opt/homebrew/bin:/usr/bin:/bin:/Users/x/.nvm/versions/node/v24/bin",
        );
        assert_eq!(added, 2, "{:?}", parts(&merged));
        assert_eq!(
            parts(&merged),
            [
                "/usr/bin",
                "/bin",
                "/opt/homebrew/bin",
                "/Users/x/.nvm/versions/node/v24/bin"
            ]
        );
    }

    /// What was inherited keeps precedence: a deliberate environment is not
    /// overruled by the profile, only extended.
    #[test]
    fn what_was_inherited_still_comes_first() {
        let (merged, _) = merge(OsStr::new("/my/tools:/usr/bin"), "/usr/bin:/my/tools");
        assert_eq!(parts(&merged), ["/my/tools", "/usr/bin"]);
    }

    /// A shell that adds nothing costs nothing, so the caller can stay silent.
    #[test]
    fn nothing_new_is_reported_as_nothing_new() {
        let (_, added) = merge(OsStr::new("/usr/bin:/bin"), "/bin:/usr/bin");
        assert_eq!(added, 0);
    }

    /// Trailing newlines and empty fields are the shape real shells answer in.
    #[test]
    fn the_answer_is_taken_as_a_shell_gives_it() {
        let (merged, added) = merge(OsStr::new("/usr/bin"), "/usr/bin:/opt/bin\n");
        assert_eq!(added, 1);
        assert_eq!(parts(&merged), ["/usr/bin", "/opt/bin"]);
    }
}
