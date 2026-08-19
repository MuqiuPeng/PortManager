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
        detect_node(&pkg, &mut frameworks, &mut services);
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
) {
    let deps: BTreeMap<String, String> = ["dependencies", "devDependencies"]
        .iter()
        .filter_map(|key| pkg.get(*key).and_then(|v| v.as_object()))
        .flat_map(|map| {
            map.iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
        })
        .collect();

    // Ordered most specific first: Next.js also depends on React.
    let known: &[(&str, &str, u16)] = &[
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

    let mut default_port = None;
    for (dep, label, port) in known {
        if deps.contains_key(*dep) {
            frameworks.push((*label).to_string());
            default_port.get_or_insert(*port);
        }
    }

    let manager = package_manager(pkg);
    let scripts = pkg.get("scripts").and_then(|v| v.as_object());
    let Some(scripts) = scripts else { return };

    // `dev` is the script a developer actually runs; `start` is the fallback.
    for (script, service_name) in [("dev", "web"), ("start", "web"), ("api", "api"), ("worker", "worker")] {
        if !scripts.contains_key(script) {
            continue;
        }
        if services.iter().any(|s| s.name == service_name) {
            continue;
        }
        let command = format!("{manager} run {script}");
        services.push(DetectedService {
            service_type: guess_type(service_name, &command),
            name: service_name.to_string(),
            port: if service_name == "web" {
                default_port
            } else {
                None
            },
            command,
            cwd: None,
            reason: format!("package.json scripts.{script}"),
        });
    }
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

fn guess_type(name: &str, command: &str) -> ServiceType {
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
