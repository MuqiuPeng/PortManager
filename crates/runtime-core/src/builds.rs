//! What starting a service would do to a build directory somebody else is using.
//!
//! Two services in one checkout can be incompatible in a way nothing about
//! ports or processes reveals. A Next project's development server and its
//! production server both write to `.next`, and the production one reads what
//! it finds there at startup — so running the development server replaces the
//! build the production one is serving from. Nothing fails at the time. The
//! failure arrives at the next restart, by which point the cause is hours old
//! and the symptom ("Could not find a production build") points at the wrong
//! service.
//!
//! This happened twice on the machine this was written on, both times because a
//! service was started under a command that looked right. Ports were free, the
//! command came off the running process, and the damage was done by the
//! difference between `NODE_ENV=production node server.mjs` and `node
//! server.mjs` — which is not visible in a process listing at all.
//!
//! Reported, never blocked. Running a development server is a normal thing to
//! want, and a runtime that refuses is a runtime people work around.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// What a start is about to do that the caller would want to know first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildHazard {
    /// It runs in production mode, and the build it needs is not there.
    ///
    /// It will keep serving if it is already up — it read what it needed at
    /// startup — and will not come back from the next restart.
    MissingProductionBuild { directory: PathBuf },
    /// It runs in development mode, in a directory something else serves from.
    ///
    /// Starting it replaces that build. The other service goes on working until
    /// something restarts it.
    OverwritesBuildUsedBy {
        directory: PathBuf,
        used_by: String,
    },
}

impl BuildHazard {
    pub fn describe(&self) -> String {
        match self {
            BuildHazard::MissingProductionBuild { directory } => format!(
                "runs in production mode but {} holds a development build; \
                 it will not start until `next build` is run",
                directory.display()
            ),
            BuildHazard::OverwritesBuildUsedBy { directory, used_by } => format!(
                "runs in development mode and will rewrite {}, which '{used_by}' \
                 serves from in production; '{used_by}' keeps working until it restarts",
                directory.display()
            ),
        }
    }
}

/// Whether a command and environment select a production build.
///
/// The environment is half the answer and the half that is invisible in a
/// process listing: `node server.mjs` is a project's development server or its
/// production one depending on `NODE_ENV` alone.
pub fn runs_in_production(command: &str, env: &BTreeMap<String, String>) -> bool {
    if env.get("NODE_ENV").map(String::as_str) == Some("production") {
        return true;
    }
    let command = command.to_ascii_lowercase();
    command.contains("node_env=production")
        || command.contains("next start")
        || command.contains(" start")
}

/// Whether a command builds as it serves.
///
/// Only the frameworks that share one directory between modes. A dev server
/// that writes somewhere else entirely cannot overwrite anything.
fn rewrites_the_build(command: &str, directory: &Path) -> bool {
    if !directory.join(".next").is_dir() {
        return false;
    }
    let command = command.to_ascii_lowercase();
    command.contains("dev") || command.contains("server.mjs") || command.contains("server.js")
}

/// Does a production build exist in this directory?
fn has_production_build(directory: &Path) -> bool {
    let next = directory.join(".next");
    !next.is_dir() || next.join("BUILD_ID").exists()
}

/// Something else that serves from a directory, for the second check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Neighbour {
    pub name: String,
    pub directory: PathBuf,
    pub production: bool,
    /// Only a neighbour that is actually up has a build it is serving from.
    pub running: bool,
}

/// What starting this service would do, if anything worth saying.
pub fn hazard(
    command: &str,
    env: &BTreeMap<String, String>,
    directory: &Path,
    neighbours: &[Neighbour],
) -> Option<BuildHazard> {
    let production = runs_in_production(command, env);

    if production && !has_production_build(directory) {
        return Some(BuildHazard::MissingProductionBuild {
            directory: directory.join(".next"),
        });
    }

    if !production && rewrites_the_build(command, directory) {
        // Only a neighbour that is running: one that is stopped has no build
        // in use, and will be rebuilt or restarted on its own terms.
        if let Some(neighbour) = neighbours
            .iter()
            .find(|other| other.production && other.running && other.directory == directory)
        {
            return Some(BuildHazard::OverwritesBuildUsedBy {
                directory: directory.join(".next"),
                used_by: neighbour.name.clone(),
            });
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn checkout(with_build_id: bool) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let next = dir.path().join(".next");
        std::fs::create_dir_all(&next).unwrap();
        if with_build_id {
            std::fs::write(next.join("BUILD_ID"), "abc").unwrap();
        }
        dir
    }

    #[test]
    fn the_environment_decides_the_mode_where_the_command_cannot() {
        // The exact pair that caused the damage: identical argv.
        assert!(runs_in_production(
            "node server.mjs",
            &env(&[("NODE_ENV", "production")])
        ));
        assert!(!runs_in_production("node server.mjs", &env(&[])));
    }

    #[test]
    fn production_without_a_production_build_is_reported() {
        let dir = checkout(false);
        let found = hazard(
            "node server.mjs",
            &env(&[("NODE_ENV", "production")]),
            dir.path(),
            &[],
        )
        .expect("a production service with a dev build is a hazard");
        assert!(matches!(found, BuildHazard::MissingProductionBuild { .. }));
        assert!(found.describe().contains("next build"));
    }

    #[test]
    fn production_with_its_build_present_is_fine() {
        let dir = checkout(true);
        assert!(hazard(
            "node server.mjs",
            &env(&[("NODE_ENV", "production")]),
            dir.path(),
            &[]
        )
        .is_none());
    }

    #[test]
    fn a_dev_server_beside_a_running_production_one_is_reported() {
        // Today's incident, in one test: starting the dev server replaced the
        // build the production server was serving from, and nothing failed
        // until the next restart.
        let dir = checkout(true);
        let neighbours = vec![Neighbour {
            name: "flip7".to_string(),
            directory: dir.path().to_path_buf(),
            production: true,
            running: true,
        }];

        let found = hazard("node server.mjs", &env(&[]), dir.path(), &neighbours)
            .expect("overwriting a build in use is a hazard");
        match &found {
            BuildHazard::OverwritesBuildUsedBy { used_by, .. } => assert_eq!(used_by, "flip7"),
            other => panic!("{other:?}"),
        }
        assert!(found.describe().contains("flip7"));
    }

    #[test]
    fn a_stopped_neighbour_has_no_build_in_use() {
        let dir = checkout(true);
        let neighbours = vec![Neighbour {
            name: "flip7".to_string(),
            directory: dir.path().to_path_buf(),
            production: true,
            running: false,
        }];
        assert!(hazard("node server.mjs", &env(&[]), dir.path(), &neighbours).is_none());
    }

    #[test]
    fn a_project_that_does_not_share_a_build_directory_is_not_warned_about() {
        // No `.next`, so the two modes cannot be writing over each other.
        let dir = tempfile::tempdir().unwrap();
        let neighbours = vec![Neighbour {
            name: "api".to_string(),
            directory: dir.path().to_path_buf(),
            production: true,
            running: true,
        }];
        assert!(hazard("python -m uvicorn app:main", &env(&[]), dir.path(), &neighbours).is_none());
    }

    #[test]
    fn a_neighbour_in_another_directory_is_not_at_risk() {
        let dir = checkout(true);
        let elsewhere = checkout(true);
        let neighbours = vec![Neighbour {
            name: "other".to_string(),
            directory: elsewhere.path().to_path_buf(),
            production: true,
            running: true,
        }];
        assert!(hazard("node server.mjs", &env(&[]), dir.path(), &neighbours).is_none());
    }
}
