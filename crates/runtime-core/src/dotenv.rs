//! `.env` files.
//!
//! Docker Compose reads them, Next reads them, Vite reads them, and a developer
//! running a service from a terminal usually has direnv or a `source` in the
//! way. A runtime that spawns the same command without them starts a *different*
//! process than the one the developer would have started, and the difference
//! shows up as a missing variable deep inside the service.
//!
//! Deliberately simple: `KEY=VALUE`, one per line, no interpolation. A file
//! that means something subtler than that is doing more than this should guess
//! at, and the service can be given explicit variables instead.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Files read for a service, in increasing order of precedence.
///
/// The workspace root first, then the service's own directory: a package in a
/// monorepo should be able to override what the repository sets.
pub const FILES: &[&str] = &[".env", ".env.local"];

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Loaded {
    pub variables: BTreeMap<String, String>,
    /// Which files contributed, for the log line.
    pub sources: Vec<PathBuf>,
}

impl Loaded {
    pub fn is_empty(&self) -> bool {
        self.variables.is_empty()
    }

    /// A short description for the service's log.
    ///
    /// Loading environment silently would make a service's behaviour depend on
    /// a file nobody mentioned.
    pub fn describe(&self) -> String {
        let files = self
            .sources
            .iter()
            .map(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.display().to_string())
            })
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "loaded {} variable{} from {files}",
            self.variables.len(),
            if self.variables.len() == 1 { "" } else { "s" }
        )
    }
}

/// Read the `.env` files that apply to a service.
pub fn load(workspace_root: &Path, service_cwd: &Path) -> Loaded {
    let mut loaded = Loaded::default();

    let mut directories = vec![workspace_root];
    if service_cwd != workspace_root {
        directories.push(service_cwd);
    }

    for directory in directories {
        for name in FILES {
            let path = directory.join(name);
            let Ok(contents) = std::fs::read_to_string(&path) else {
                continue;
            };
            let parsed = parse(&contents);
            if parsed.is_empty() {
                continue;
            }
            loaded.variables.extend(parsed);
            loaded.sources.push(path);
        }
    }
    loaded
}

/// Parse `KEY=VALUE` lines.
pub fn parse(contents: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();

    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // `export FOO=bar` is common in files meant to be sourced as well.
        let line = line.strip_prefix("export ").unwrap_or(line);

        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() || !key.chars().all(|c| c.is_alphanumeric() || c == '_') {
            continue;
        }

        out.insert(key.to_string(), unquote(value.trim()));
    }
    out
}

/// Strip one layer of matching quotes, and an unquoted trailing comment.
fn unquote(value: &str) -> String {
    for quote in ['"', '\''] {
        if value.len() >= 2 && value.starts_with(quote) && value.ends_with(quote) {
            return value[1..value.len() - 1].to_string();
        }
    }
    // Only outside quotes: a `#` inside a quoted value is part of it, and
    // passwords contain `#` more often than anyone would like.
    value
        .split_once(" #")
        .map(|(before, _)| before.trim_end())
        .unwrap_or(value)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_shapes_a_dotenv_file_actually_has() {
        let parsed = parse(
            r#"
            # a comment
            DATABASE_URL=postgres://localhost:5432/app
            export TOKEN="secret value"
            QUOTED='single'
            EMPTY=
            TRAILING=value # explanation
            not a variable
            "#,
        );

        assert_eq!(
            parsed.get("DATABASE_URL").map(String::as_str),
            Some("postgres://localhost:5432/app")
        );
        assert_eq!(parsed.get("TOKEN").map(String::as_str), Some("secret value"));
        assert_eq!(parsed.get("QUOTED").map(String::as_str), Some("single"));
        assert_eq!(parsed.get("EMPTY").map(String::as_str), Some(""));
        assert_eq!(parsed.get("TRAILING").map(String::as_str), Some("value"));
        assert!(!parsed.contains_key("not a variable"));
    }

    #[test]
    fn a_hash_inside_a_quoted_value_is_part_of_it() {
        // Passwords contain `#` more often than anyone would like.
        let parsed = parse(r#"PASSWORD="p#ss word""#);
        assert_eq!(parsed.get("PASSWORD").map(String::as_str), Some("p#ss word"));
    }

    #[test]
    fn a_package_overrides_the_repository() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let package = root.join("packages").join("api");
        std::fs::create_dir_all(&package).unwrap();

        std::fs::write(root.join(".env"), "SHARED=root\nONLY_ROOT=yes\n").unwrap();
        std::fs::write(package.join(".env"), "SHARED=package\n").unwrap();

        let loaded = load(root, &package);

        assert_eq!(loaded.variables.get("SHARED").map(String::as_str), Some("package"));
        assert_eq!(loaded.variables.get("ONLY_ROOT").map(String::as_str), Some("yes"));
        assert_eq!(loaded.sources.len(), 2);
    }

    #[test]
    fn local_overrides_the_committed_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".env"), "MODE=committed\n").unwrap();
        std::fs::write(dir.path().join(".env.local"), "MODE=local\n").unwrap();

        let loaded = load(dir.path(), dir.path());
        assert_eq!(loaded.variables.get("MODE").map(String::as_str), Some("local"));
    }
}
