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
use std::ffi::OsString;
#[cfg(unix)]
use std::time::Duration;

/// How long the login shell gets. A profile that takes longer than this is one
/// that would make every launch feel broken, and a short `PATH` is better than
/// a daemon that never finishes starting.
#[cfg(unix)]
const PATIENCE: Duration = Duration::from_secs(3);

/// Ask the shell where it would look, and resolve commands the way it does.
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

    let mut sources: Vec<OsString> = Vec::new();
    // The interactive shell first, then the login shell, and only then what we
    // inherited. Order is the whole mechanism of a version manager: nvm works
    // by putting the chosen version's directory in front of the system one, so
    // appending its answer — which is what this did — hands that back.
    //
    // It cost a real failure. The daemon resolved `node` to /usr/local/bin,
    // which is v22, while every shell on this machine resolves it to nvm's
    // v24; a Prisma migration then died inside a dependency that v22 cannot
    // load, reporting an ESM error that says nothing about which node ran it.
    //
    // What is inherited comes last rather than not at all. From a terminal it
    // is the same list the shell just gave us, so the order does not change;
    // from Finder it is `/usr/bin:/bin:/usr/sbin:/sbin`, which is not a choice
    // anybody made and should not outrank one.
    if let Some(answer) = interactive {
        sources.push(OsString::from(answer));
    }
    if let Some(answer) = login {
        sources.push(OsString::from(answer));
    }
    if sources.is_empty() {
        return;
    }
    sources.push(inherited.clone());

    let merged = merge(&sources);
    if merged == inherited {
        return;
    }
    // Logged rather than silent: a daemon that quietly rewrites its own
    // environment is one nobody can explain the behaviour of later.
    tracing::info!(
        entries = std::env::split_paths(&merged).count(),
        "took the PATH the shell resolves commands against"
    );
    std::env::set_var("PATH", merged);
}

/// What the shell prints before the answer, so a talkative profile does not
/// become part of it.
///
/// An interactive shell runs the rc file, and rc files greet people, print
/// tips, and warn about updates. Taking the whole of stdout would fold that
/// into `PATH`.
#[cfg(unix)]
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
    // Trimmed here rather than in `merge`: a shell ends its output with a
    // newline, and an untrimmed answer turns the last directory into one that
    // does not exist — which fails later as "command not found", pointing
    // nowhere near this.
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

/// Every source in turn, first mention winning, nothing repeated.
///
/// Order is the point rather than a detail: whichever source is listed first
/// decides which of two directories holding the same command is the one that
/// runs.
fn merge(sources: &[OsString]) -> OsString {
    let mut seen: HashSet<OsString> = HashSet::new();
    let mut kept: Vec<OsString> = Vec::new();

    for source in sources {
        for entry in std::env::split_paths(source) {
            let entry = entry.into_os_string();
            if !entry.is_empty() && seen.insert(entry.clone()) {
                kept.push(entry);
            }
        }
    }
    std::env::join_paths(kept.iter()).unwrap_or_else(|_| {
        sources.last().cloned().unwrap_or_default()
    })
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::*;

    /// What separates two directories on this platform.
    ///
    /// The tests below used to spell it `:`, which is one entry on Windows and
    /// two everywhere else — so every one of them passed here and failed
    /// there, asserting nothing about the code and a great deal about the
    /// machine it ran on.
    const SEP: &str = if cfg!(windows) { ";" } else { ":" };

    /// A PATH built the way the platform writes one.
    fn os(parts: &[&str]) -> OsString {
        OsString::from(parts.join(SEP))
    }

    fn parts(v: &OsStr) -> Vec<String> {
        std::env::split_paths(v)
            .map(|p| p.to_string_lossy().into_owned())
            .collect()
    }

    /// A version manager works by going in front. Appending its answer is the
    /// same as ignoring it.
    ///
    /// This is not hypothetical: with the sources in the other order the daemon
    /// resolved `node` to /usr/local/bin — v22 on the machine this was found on
    /// — while every shell there resolves it to nvm's v24, and a Prisma
    /// migration died inside a dependency the older one cannot load.
    #[test]
    fn the_version_manager_keeps_the_front() {
        let merged = merge(&[
            os(&["/nvm/v24/bin", "/usr/local/bin", "/usr/bin"]),
            os(&["/usr/local/bin", "/usr/bin"]),
            os(&["/usr/bin", "/bin"]),
        ]);
        assert_eq!(
            parts(&merged).first().map(String::as_str),
            Some("/nvm/v24/bin"),
            "{:?}",
            parts(&merged)
        );
    }

    /// What a Finder launch has is not a preference, so it does not outrank the
    /// shell — but nothing in it is dropped either.
    #[test]
    fn nothing_inherited_is_lost_by_being_outranked() {
        let merged = merge(&[
            os(&["/opt/homebrew/bin"]),
            os(&["/usr/bin", "/bin", "/usr/sbin", "/sbin"]),
        ]);
        assert_eq!(
            parts(&merged),
            ["/opt/homebrew/bin", "/usr/bin", "/bin", "/usr/sbin", "/sbin"]
        );
    }

    /// One entry, however many sources name it.
    #[test]
    fn a_directory_named_twice_appears_once() {
        let merged = merge(&[
            os(&["/usr/bin", "/bin"]),
            os(&["/bin", "/usr/bin", "/opt/bin"]),
        ]);
        assert_eq!(parts(&merged), ["/usr/bin", "/bin", "/opt/bin"]);
    }

    /// An empty entry means the working directory, which is not a place any
    /// command should be found from.
    #[test]
    fn an_empty_source_adds_nothing() {
        let merged = merge(&[os(&[]), os(&["/usr/bin", "", "/bin"]), os(&[])]);
        assert_eq!(parts(&merged), ["/usr/bin", "/bin"]);
    }
}
