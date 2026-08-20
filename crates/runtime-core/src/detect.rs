//! Project detection.
//!
//! Everything inferred here is a *suggestion*. The daemon writes it into the
//! registry so the user can correct it, and a committed `.runtime.json` always
//! wins over inference.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use runtime_types::{ProjectConfig, ServiceConfig, ServiceType, CONFIG_FILE_NAME};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Detection {
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frameworks: Vec<String>,
    pub services: Vec<DetectedService>,
    /// True when the result came from a committed `.runtime.json`.
    pub from_config: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectedService {
    pub name: String,
    pub service_type: ServiceType,
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    /// Why this was suggested, shown in the CLI and the add-project dialog.
    pub reason: String,
}

/// Inspect a directory and propose a project with its services.
pub fn detect(root: &Path) -> Detection {
    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "project".to_string());

    if let Some(config) = read_config(root) {
        return from_config(&name, root, config);
    }

    let mut frameworks = Vec::new();
    let mut services = Vec::new();
    let mut name = name;

    if let Some(pkg) = read_json(&root.join("package.json")) {
        // The declared package name is what the developer calls the project;
        // the directory name is only a fallback.
        if let Some(declared) = pkg.get("name").and_then(|value| value.as_str()) {
            if !declared.trim().is_empty() {
                name = declared.trim().to_string();
            }
        }
        // Workspace members first: they are the real runnable units, and a
        // root script that merely forwards to one should not shadow it.
        let (members, member_tokens) = detect_workspace_members(root, &pkg, &mut frameworks);
        detect_node(&pkg, &mut frameworks, &mut services, &member_tokens);
        services.extend(members);
    }
    detect_python(root, &mut frameworks, &mut services);
    detect_rust(root, &mut frameworks, &mut services);
    detect_compose(root, &mut frameworks, &mut services);

    Detection {
        name,
        frameworks,
        services,
        from_config: false,
    }
}

fn read_config(root: &Path) -> Option<ProjectConfig> {
    let raw = std::fs::read_to_string(root.join(CONFIG_FILE_NAME)).ok()?;
    match serde_json::from_str(&raw) {
        Ok(config) => Some(config),
        Err(err) => {
            tracing::warn!(path = %root.display(), %err, "ignoring malformed {CONFIG_FILE_NAME}");
            None
        }
    }
}

fn from_config(fallback_name: &str, root: &Path, config: ProjectConfig) -> Detection {
    let services = config
        .services
        .into_iter()
        .map(|(name, service): (String, ServiceConfig)| DetectedService {
            service_type: service
                .service_type
                .unwrap_or_else(|| guess_type(&name, &service.command)),
            name,
            command: service.command,
            port: service.port,
            cwd: service.cwd.map(|cwd| {
                if cwd.is_absolute() {
                    cwd
                } else {
                    root.join(cwd)
                }
            }),
            reason: CONFIG_FILE_NAME.to_string(),
        })
        .collect();

    Detection {
        name: config.name.unwrap_or_else(|| fallback_name.to_string()),
        frameworks: Vec::new(),
        services,
        from_config: true,
    }
}

fn read_json(path: &Path) -> Option<serde_json::Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn detect_node(
    pkg: &serde_json::Value,
    frameworks: &mut Vec<String>,
    services: &mut Vec<DetectedService>,
    member_tokens: &[String],
) {
    let default_port = framework_port(pkg, frameworks);

    let manager = package_manager(pkg);
    let scripts = pkg.get("scripts").and_then(|v| v.as_object());
    let Some(scripts) = scripts else { return };

    // `dev` is the script a developer actually runs; `start` is the fallback.
    let mut candidates: Vec<(String, String)> = Vec::new();
    for (script, service_name) in [
        ("dev", "web"),
        ("start", "web"),
        ("api", "api"),
        ("worker", "worker"),
    ] {
        if scripts.contains_key(script) {
            candidates.push((script.to_string(), service_name.to_string()));
        }
    }

    // Monorepos name their scripts after the thing they run: `api:dev`,
    // `scheduler:dev`, `web:dev`. Without these a workspace root with a dozen
    // scripts is detected as having no services at all.
    for script in scripts.keys() {
        let Some(name) = script
            .strip_suffix(":dev")
            .or_else(|| script.strip_prefix("dev:"))
        else {
            continue;
        };
        if name.is_empty() || !is_service_like(name) {
            continue;
        }
        candidates.push((script.clone(), name.to_string()));
    }

    for (script, service_name) in candidates {
        if services.iter().any(|s| s.name == service_name) {
            continue;
        }
        // A root script that forwards to a workspace member starts the same
        // thing the member does, under a different name and rooted at the
        // repository rather than the package. Keeping both would double the
        // service list and leave neither able to recognise the running process.
        let body = scripts
            .get(&script)
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        if forwards_to_member(body, member_tokens) {
            continue;
        }
        let command = format!("{manager} run {script}");
        services.push(DetectedService {
            service_type: guess_type(&service_name, &command),
            port: if service_name == "web" {
                default_port
            } else {
                None
            },
            name: service_name,
            command,
            cwd: None,
            reason: format!("package.json scripts.{script}"),
        });
    }
}

/// Actions that happen to be spelled like a `:dev` script but do not start a
/// local server.
///
/// `deploy:dev` deploys to a development *environment*. Offering it as a
/// startable service would let a single "start deploy" ship code — the kind of
/// default that has to be wrong-by-construction, not merely discouraged.
const NON_SERVICE_SCRIPTS: &[&str] = &[
    "deploy",
    "remove",
    "destroy",
    "publish",
    "release",
    "build",
    "test",
    "e2e",
    "lint",
    "format",
    "typecheck",
    "check",
    "clean",
    "migrate",
    "seed",
    "generate",
    "codegen",
    "install",
    "setup",
    "prepare",
    "db",
];

fn is_service_like(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    !NON_SERVICE_SCRIPTS
        .iter()
        .any(|action| name == *action || name.starts_with(&format!("{action}:")))
}

/// The framework a manifest declares, and the port it defaults to.
///
/// Ordered most specific first: Next.js also depends on React.
fn framework_port(pkg: &serde_json::Value, frameworks: &mut Vec<String>) -> Option<u16> {
    const KNOWN: &[(&str, &str, u16)] = &[
        ("next", "Next.js", 3000),
        ("nuxt", "Nuxt", 3000),
        ("@remix-run/dev", "Remix", 3000),
        ("@sveltejs/kit", "SvelteKit", 5173),
        ("astro", "Astro", 4321),
        ("@angular/cli", "Angular", 4200),
        ("react-scripts", "Create React App", 3000),
        ("vite", "Vite", 5173),
        ("@nestjs/core", "NestJS", 3000),
        ("express", "Express", 3000),
        ("fastify", "Fastify", 3000),
    ];

    let deps: BTreeMap<String, String> = ["dependencies", "devDependencies"]
        .iter()
        .filter_map(|key| pkg.get(*key).and_then(|value| value.as_object()))
        .flat_map(|map| {
            map.iter()
                .map(|(key, value)| (key.clone(), value.as_str().unwrap_or_default().to_string()))
        })
        .collect();

    let mut port = None;
    for (dependency, label, default) in KNOWN {
        if deps.contains_key(*dependency) {
            let label = (*label).to_string();
            if !frameworks.contains(&label) {
                frameworks.push(label);
            }
            port.get_or_insert(*default);
        }
    }
    port
}

/// Services from the packages of a monorepo.
///
/// Reading only the root manifest misses them entirely: a workspace root often
/// has nothing but `build` and `lint`, and every dev server lives in
/// `packages/*`. Even when the root does forward — `api:dev` running
/// `pnpm --filter @acme/payments dev` — the service it produces is named after
/// the script and rooted at the repository, so its working directory does not
/// match the process that actually runs, and it can never be recognised as
/// already running.
fn detect_workspace_members(
    root: &Path,
    pkg: &serde_json::Value,
    frameworks: &mut Vec<String>,
) -> (Vec<DetectedService>, Vec<String>) {
    let patterns = workspace_patterns(root, pkg);
    if patterns.is_empty() {
        return (Vec::new(), Vec::new());
    }

    let mut services = Vec::new();
    // Both spellings a root script might use to name a member.
    let mut tokens = Vec::new();
    for directory in expand_patterns(root, &patterns) {
        let Some(member) = read_json(&directory.join("package.json")) else {
            continue;
        };
        if let Some(declared) = member.get("name").and_then(|value| value.as_str()) {
            tokens.push(declared.to_string());
        }
        if let Some(directory_name) = directory.file_name() {
            tokens.push(directory_name.to_string_lossy().to_string());
        }
        let Some(scripts) = member.get("scripts").and_then(|value| value.as_object()) else {
            continue;
        };
        // `dev` is what a developer runs; `start` is the fallback.
        let Some(script) = ["dev", "start"]
            .into_iter()
            .find(|candidate| scripts.contains_key(*candidate))
        else {
            continue;
        };

        let name = directory
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "app".to_string());
        let manager = package_manager(pkg);
        let command = format!("{manager} run {script}");

        let mut member_frameworks = Vec::new();
        let port = framework_port(&member, &mut member_frameworks);
        for framework in member_frameworks {
            if !frameworks.contains(&framework) {
                frameworks.push(framework);
            }
        }

        services.push(DetectedService {
            service_type: guess_type(&name, &command),
            name,
            command,
            port,
            // The member's own directory, which is where its process runs — the
            // difference between recognising it later and not.
            cwd: Some(directory),
            reason: "workspace member".to_string(),
        });
    }
    (services, tokens)
}

/// Whether a root script just runs a workspace member.
fn forwards_to_member(script_body: &str, member_tokens: &[String]) -> bool {
    if member_tokens.is_empty() {
        return false;
    }
    // Naming a member: `pnpm --filter @acme/payments dev`, `yarn workspace ...`.
    if member_tokens
        .iter()
        .any(|token| script_body.contains(token.as_str()))
    {
        return true;
    }
    // Or running all of them at once, which the members already cover.
    ["turbo", "nx ", "lerna", "pnpm -r ", "pnpm --recursive"]
        .iter()
        .any(|runner| script_body.contains(runner))
}

/// Workspace globs from `pnpm-workspace.yaml` or the `workspaces` field.
fn workspace_patterns(root: &Path, pkg: &serde_json::Value) -> Vec<String> {
    // pnpm keeps them in its own file.
    if let Ok(raw) = std::fs::read_to_string(root.join("pnpm-workspace.yaml")) {
        let patterns = parse_pnpm_workspace(&raw);
        if !patterns.is_empty() {
            return patterns;
        }
    }

    // npm, yarn and bun use `workspaces`, as either a list or `{ packages }`.
    let workspaces = pkg.get("workspaces");
    let list = workspaces
        .and_then(|value| value.as_array())
        .or_else(|| workspaces?.get("packages")?.as_array());

    list.map(|entries| {
        entries
            .iter()
            .filter_map(|entry| entry.as_str().map(str::to_string))
            .collect()
    })
    .unwrap_or_default()
}

/// The `packages:` list from `pnpm-workspace.yaml`.
///
/// Hand-parsed rather than pulling in a YAML crate: the shape is a fixed list
/// of strings, and this is the only YAML the runtime ever reads.
fn parse_pnpm_workspace(raw: &str) -> Vec<String> {
    let mut patterns = Vec::new();
    let mut in_packages = false;

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with("packages:") {
            in_packages = true;
            continue;
        }
        if !in_packages {
            continue;
        }
        let Some(entry) = trimmed.strip_prefix("- ") else {
            // Any other top-level key ends the list.
            if !line.starts_with(' ') && !line.starts_with('-') {
                in_packages = false;
            }
            continue;
        };
        patterns.push(entry.trim().trim_matches(['"', '\'']).to_string());
    }
    patterns
}

/// Directories matching workspace globs, supporting `*` and `**`.
fn expand_patterns(root: &Path, patterns: &[String]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for pattern in patterns {
        let segments: Vec<&str> = pattern
            .split('/')
            .filter(|segment| !segment.is_empty() && *segment != ".")
            .collect();
        expand_segments(root, &segments, &mut out);
    }
    out.sort();
    out.dedup();
    out
}

fn expand_segments(directory: &Path, segments: &[&str], out: &mut Vec<PathBuf>) {
    let Some((head, rest)) = segments.split_first() else {
        out.push(directory.to_path_buf());
        return;
    };

    match *head {
        // Bounded: a workspace glob is not an invitation to walk a home
        // directory, and `**` in practice means "a level or two".
        "**" => {
            expand_segments(directory, rest, out);
            for child in child_directories(directory) {
                expand_segments(&child, segments, out);
            }
        }
        "*" => {
            for child in child_directories(directory) {
                expand_segments(&child, rest, out);
            }
        }
        literal => {
            let child = directory.join(literal);
            if child.is_dir() {
                expand_segments(&child, rest, out);
            }
        }
    }
}

fn child_directories(directory: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .filter(|path| {
            path.file_name()
                .map(|name| {
                    let name = name.to_string_lossy();
                    !name.starts_with('.') && name != "node_modules"
                })
                .unwrap_or(false)
        })
        .collect()
}

fn package_manager(pkg: &serde_json::Value) -> &'static str {
    match pkg.get("packageManager").and_then(|v| v.as_str()) {
        Some(value) if value.starts_with("pnpm") => "pnpm",
        Some(value) if value.starts_with("yarn") => "yarn",
        Some(value) if value.starts_with("bun") => "bun",
        _ => "npm",
    }
}

fn detect_python(root: &Path, frameworks: &mut Vec<String>, services: &mut Vec<DetectedService>) {
    let manifests = ["pyproject.toml", "requirements.txt"];
    let text: String = manifests
        .iter()
        .filter_map(|name| std::fs::read_to_string(root.join(name)).ok())
        .collect::<Vec<_>>()
        .join("\n");
    if text.is_empty() {
        return;
    }

    if root.join("manage.py").exists() {
        frameworks.push("Django".to_string());
        services.push(DetectedService {
            name: "api".to_string(),
            service_type: ServiceType::Api,
            command: "python manage.py runserver".to_string(),
            port: Some(8000),
            cwd: None,
            reason: "manage.py".to_string(),
        });
        return;
    }

    if text.contains("fastapi") || text.contains("uvicorn") {
        frameworks.push("FastAPI".to_string());
        services.push(DetectedService {
            name: "api".to_string(),
            service_type: ServiceType::Api,
            command: "uvicorn app.main:app --reload".to_string(),
            port: Some(8000),
            cwd: None,
            reason: "fastapi/uvicorn dependency".to_string(),
        });
    } else if text.contains("flask") {
        frameworks.push("Flask".to_string());
        services.push(DetectedService {
            name: "api".to_string(),
            service_type: ServiceType::Api,
            command: "flask run".to_string(),
            port: Some(5000),
            cwd: None,
            reason: "flask dependency".to_string(),
        });
    }
}

fn detect_rust(root: &Path, frameworks: &mut Vec<String>, services: &mut Vec<DetectedService>) {
    let manifest = root.join("Cargo.toml");
    if !manifest.exists() {
        return;
    }
    frameworks.push("Cargo".to_string());
    // Only a workspace member with a binary is runnable, and reading that
    // reliably means parsing the manifest; suggest the obvious command and let
    // the user correct it.
    if !services.iter().any(|s| s.name == "app") {
        services.push(DetectedService {
            name: "app".to_string(),
            service_type: ServiceType::Custom,
            command: "cargo run".to_string(),
            port: None,
            cwd: None,
            reason: "Cargo.toml".to_string(),
        });
    }
}

fn detect_compose(root: &Path, frameworks: &mut Vec<String>, services: &mut Vec<DetectedService>) {
    let found = ["docker-compose.yml", "docker-compose.yaml", "compose.yml", "compose.yaml"]
        .iter()
        .any(|name| root.join(name).exists());
    if !found {
        return;
    }
    frameworks.push("Docker Compose".to_string());
    // Compose services are managed as one unit until the Docker provider lands.
    services.push(DetectedService {
        name: "compose".to_string(),
        service_type: ServiceType::Container,
        command: "docker compose up".to_string(),
        port: None,
        cwd: None,
        reason: "docker compose file".to_string(),
    });
}

/// What kind of thing a service is, from its name and command.
///
/// A guess, and a correctable one — but no longer only cosmetic. The type
/// decides how the service is checked when it does not say: anything claiming
/// to serve HTTP is asked to answer, and everything else is only asked to hold
/// its port. So a service declared by hand without a type used to fall to
/// `Custom` and quietly get the weaker check, which is the opposite of what
/// somebody typing `--command "npm run dev"` means.
pub fn guess_type(name: &str, command: &str) -> ServiceType {
    let haystack = format!("{name} {command}").to_ascii_lowercase();
    if haystack.contains("worker") || haystack.contains("queue") {
        ServiceType::Worker
    } else if haystack.contains("postgres") || haystack.contains("mysql") {
        ServiceType::Database
    } else if haystack.contains("redis") {
        ServiceType::Cache
    } else if haystack.contains("docker") || haystack.contains("compose") {
        ServiceType::Container
    } else if name == "api" || haystack.contains("uvicorn") || haystack.contains("server") {
        ServiceType::Api
    } else if name == "web" || haystack.contains("dev") {
        ServiceType::Web
    } else {
        ServiceType::Custom
    }
}

#[cfg(test)]
mod tests {
    /// The type is no longer only cosmetic: it decides whether a service is
    /// asked to answer or only to hold its port. These are the shapes that
    /// reach `guess_type` from the paths that used to hardcode or default it.
    #[test]
    fn a_dev_server_is_recognised_as_serving_http() {
        for (name, command) in [
            ("dashboard", "npm run dev"),
            ("web", "pnpm dev"),
            ("frontend", "next dev --port 3001"),
        ] {
            assert!(
                matches!(guess_type(name, command), ServiceType::Web | ServiceType::Api),
                "{name}: {command}"
            );
        }
    }

    #[test]
    fn a_database_is_not_asked_for_a_web_page() {
        // Adopting one as Web would report it broken for declining to serve
        // HTTP, and a check wrong in that direction teaches the reader to
        // skip it.
        for (name, command) in [
            ("db", "postgres -D /var/lib/postgresql"),
            ("cache", "redis-server"),
            ("worker", "pnpm run queue:worker"),
        ] {
            assert!(
                !matches!(guess_type(name, command), ServiceType::Web | ServiceType::Api),
                "{name}: {command}"
            );
        }
    }

    use super::*;

    #[test]
    fn monorepo_dev_scripts_become_services() {
        assert!(is_service_like("api"));
        assert!(is_service_like("scheduler"));
        assert!(is_service_like("web"));
    }

    #[test]
    fn deployment_scripts_never_become_services() {
        // `deploy:dev` ships code. A runtime that offers it as something to
        // start is one agent instruction away from a deployment nobody asked
        // for, so this is excluded by construction rather than by warning.
        assert!(!is_service_like("deploy"));
        assert!(!is_service_like("deploy:scheduler"));
        assert!(!is_service_like("remove"));
        assert!(!is_service_like("publish"));
        assert!(!is_service_like("db"));
    }
}
