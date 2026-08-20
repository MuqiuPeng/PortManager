//! `.runtime.json` — the per-project committed declaration.
//!
//! Kept deliberately flat: a name, and a map of services. Anything the runtime
//! can infer (cwd, type, health) is optional.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::model::{ConflictPolicy, HealthCheck, ServiceType};

/// The file name looked for at a project root.
pub const CONFIG_FILE_NAME: &str = ".runtime.json";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default)]
    pub services: BTreeMap<String, ServiceConfig>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceConfig {
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// Relative to the workspace root when not absolute.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub service_type: Option<ServiceType>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<HealthCheck>,
    #[serde(default)]
    pub auto_start: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_conflict: Option<ConflictPolicy>,
    /// Services in the same checkout that must be up first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    /// Runs to completion instead of staying up: a migration, a seed, a build.
    #[serde(default)]
    pub one_shot: bool,
}
