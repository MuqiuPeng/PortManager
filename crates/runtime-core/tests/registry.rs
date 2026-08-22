//! Integration tests for the parts of the core that are easy to get subtly
//! wrong: what gets inferred, how worktrees are numbered, and what the port
//! allocator does when it cannot have what it asked for.

use std::net::TcpListener;
use std::path::Path;
use std::process::Command;

use runtime_core::Runtime;
use runtime_types::{ConflictPolicy, RuntimeError, ServiceId, ServiceType};
use tempfile::TempDir;

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .expect("git must be installed to run these tests");
    assert!(status.success(), "git {args:?} failed");
}

/// A repository with one commit, so worktrees can be created from it.
fn repo(files: &[(&str, &str)]) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (name, contents) in files {
        std::fs::write(dir.path().join(name), contents).unwrap();
    }
    git(dir.path(), &["init", "-q"]);
    git(dir.path(), &["config", "user.email", "test@example.com"]);
    git(dir.path(), &["config", "user.name", "test"]);
    git(dir.path(), &["add", "-A"]);
    git(dir.path(), &["commit", "-qm", "init"]);
    dir
}

const PACKAGE_JSON: &str = r#"{
  "name": "shop",
  "packageManager": "pnpm@9.0.0",
  "scripts": { "dev": "next dev" },
  "dependencies": { "next": "14.0.0" }
}"#;

#[test]
fn infers_the_project_name_framework_and_port_from_package_json() {
    let dir = repo(&[("package.json", PACKAGE_JSON)]);
    let runtime = Runtime::in_memory().unwrap();

    let view = runtime.add_project(dir.path(), None).unwrap();

    assert_eq!(view.project.name, "shop");
    let services = &view.workspaces[0].services;
    assert_eq!(services.len(), 1);
    assert_eq!(services[0].service.name, "web");
    assert_eq!(services[0].service.command, "pnpm run dev");
    // Next.js defaults to 3000, and that is what the service should ask for.
    assert_eq!(services[0].service.preferred_port, Some(3000));
}

#[test]
fn a_committed_runtime_json_overrides_inference() {
    let config = r#"{
      "name": "checkout",
      "services": {
        "api": { "command": "uv run api", "port": 8080, "type": "api" }
      }
    }"#;
    let dir = repo(&[("package.json", PACKAGE_JSON), (".runtime.json", config)]);
    let runtime = Runtime::in_memory().unwrap();

    let view = runtime.add_project(dir.path(), None).unwrap();

    assert_eq!(view.project.name, "checkout");
    let services = &view.workspaces[0].services;
    assert_eq!(services.len(), 1, "inference must not add to the config");
    assert_eq!(services[0].service.name, "api");
    assert_eq!(services[0].service.preferred_port, Some(8080));
    assert_eq!(services[0].service.service_type, ServiceType::Api);
}

#[test]
fn each_worktree_gets_its_own_port_offset_and_a_copy_of_the_services() {
    let dir = repo(&[("package.json", PACKAGE_JSON)]);
    let parent = dir.path().parent().unwrap().to_path_buf();
    let worktree = parent.join("shop-refund");
    git(
        dir.path(),
        &[
            "worktree",
            "add",
            "-q",
            worktree.to_str().unwrap(),
            "-b",
            "feature/refund",
        ],
    );

    let runtime = Runtime::in_memory().unwrap();
    let view = runtime.add_project(dir.path(), None).unwrap();

    assert_eq!(view.workspaces.len(), 2);
    let main = &view.workspaces[0];
    let branch = &view.workspaces[1];
    assert!(!main.workspace.worktree);
    assert!(branch.workspace.worktree);
    assert_eq!(main.workspace.port_offset, 0);
    assert_eq!(branch.workspace.port_offset, 1);
    assert_eq!(branch.workspace.git_branch.as_deref(), Some("feature/refund"));
    assert_eq!(branch.services.len(), 1, "services are copied into worktrees");

    // The offset is what makes `main` keep 3000 while the branch takes 3001.
    let workspace = branch.workspace.clone();
    let service = branch.services[0].service.clone();
    assert_eq!(
        runtime_core::ports::PortResolver::preferred_port(&service, &workspace),
        Some(3001)
    );

    let _ = std::fs::remove_dir_all(&worktree);
}

#[test]
fn a_bare_service_name_resolves_to_the_primary_checkout() {
    let dir = repo(&[("package.json", PACKAGE_JSON)]);
    let parent = dir.path().parent().unwrap().to_path_buf();
    let worktree = parent.join("shop-resolve");
    git(
        dir.path(),
        &[
            "worktree",
            "add",
            "-q",
            worktree.to_str().unwrap(),
            "-b",
            "feature/resolve",
        ],
    );

    let runtime = Runtime::in_memory().unwrap();
    let view = runtime.add_project(dir.path(), None).unwrap();
    let project = view.project.clone();

    let main = runtime.resolve_service(Some(&project), "web").unwrap();
    let main_workspace = runtime.require_workspace(&main.workspace_id).unwrap();
    assert!(!main_workspace.worktree);

    // A branch-qualified name reaches the worktree, and the branch itself
    // contains a slash — the split has to come from the right.
    let branch = runtime
        .resolve_service(Some(&project), "feature/resolve/web")
        .unwrap();
    let branch_workspace = runtime.require_workspace(&branch.workspace_id).unwrap();
    assert!(branch_workspace.worktree);
    assert_ne!(main.id, branch.id);

    let _ = std::fs::remove_dir_all(&worktree);
}

#[test]
fn allocate_next_skips_a_port_that_is_already_bound() {
    let occupied = TcpListener::bind("127.0.0.1:0").unwrap();
    let taken = occupied.local_addr().unwrap().port();

    let config = format!(
        r#"{{ "name": "conflict", "services": {{ "web": {{ "command": "true", "port": {taken} }} }} }}"#
    );
    let dir = repo(&[(".runtime.json", config.as_str())]);

    let runtime = Runtime::in_memory().unwrap();
    let view = runtime.add_project(dir.path(), None).unwrap();
    let workspace = view.workspaces[0].workspace.clone();
    let service = view.workspaces[0].services[0].service.clone();

    let reservation = runtime
        .resolver()
        .reserve(
            &view.project,
            &workspace,
            &service,
            None,
            Some(ConflictPolicy::AllocateNext),
            Default::default(),
        )
        .unwrap();

    assert!(reservation.reallocated);
    assert_eq!(reservation.preferred_port, Some(taken));
    assert!(reservation.port > taken);
}

#[test]
fn kill_existing_refuses_a_process_the_runtime_did_not_start() {
    let occupied = TcpListener::bind("127.0.0.1:0").unwrap();
    let taken = occupied.local_addr().unwrap().port();

    let config = format!(
        r#"{{ "name": "safety", "services": {{ "web": {{ "command": "true", "port": {taken} }} }} }}"#
    );
    let dir = repo(&[(".runtime.json", config.as_str())]);

    let runtime = Runtime::in_memory().unwrap();
    let view = runtime.add_project(dir.path(), None).unwrap();
    let workspace = view.workspaces[0].workspace.clone();
    let service = view.workspaces[0].services[0].service.clone();

    let error = runtime
        .resolver()
        .reserve(
            &view.project,
            &workspace,
            &service,
            None,
            Some(ConflictPolicy::KillExisting),
            Default::default(),
        )
        .unwrap_err();

    // Never terminating an unknown process is the safety property the whole
    // conflict design depends on, so it is asserted rather than assumed.
    assert!(
        matches!(error, RuntimeError::NotPermitted { .. }),
        "expected a refusal, got {error:?}"
    );
}

#[test]
fn fail_policy_reports_who_holds_the_port() {
    let occupied = TcpListener::bind("127.0.0.1:0").unwrap();
    let taken = occupied.local_addr().unwrap().port();

    let config = format!(
        r#"{{ "name": "report", "services": {{ "web": {{ "command": "true", "port": {taken} }} }} }}"#
    );
    let dir = repo(&[(".runtime.json", config.as_str())]);

    let runtime = Runtime::in_memory().unwrap();
    let view = runtime.add_project(dir.path(), None).unwrap();
    let workspace = view.workspaces[0].workspace.clone();
    let service = view.workspaces[0].services[0].service.clone();

    let error = runtime
        .resolver()
        .reserve(
            &view.project,
            &workspace,
            &service,
            None,
            Some(ConflictPolicy::Fail),
            Default::default(),
        )
        .unwrap_err();

    match error {
        RuntimeError::PortConflict { port, holder } => {
            assert_eq!(port, taken);
            assert!(!holder.is_empty());
        }
        other => panic!("expected a port conflict, got {other:?}"),
    }
}

#[test]
fn a_name_shared_by_two_projects_is_refused_rather_than_guessed() {
    // Two unrelated checkouts can carry the same package name — this machine
    // has StockViewer and OnlineStockViewer, both called "stockviewer".
    let one = repo(&[("package.json", PACKAGE_JSON)]);
    let two = repo(&[("package.json", PACKAGE_JSON)]);

    let runtime = Runtime::in_memory().unwrap();
    runtime.add_project(one.path(), None).unwrap();
    runtime.add_project(two.path(), None).unwrap();

    let error = runtime.resolve_project("shop").unwrap_err();
    match error {
        RuntimeError::InvalidInput { message } => {
            assert!(message.contains("several projects"), "{message}");
        }
        other => panic!("expected an ambiguity error, got {other:?}"),
    }

    // A path still resolves precisely.
    let resolved = runtime
        .resolve_project(&one.path().to_string_lossy())
        .unwrap();
    assert_eq!(resolved.root_path.canonicalize().unwrap(), one.path().canonicalize().unwrap());
}

#[test]
fn a_service_name_shared_by_two_projects_is_refused() {
    let one = repo(&[("package.json", PACKAGE_JSON)]);
    let two = repo(&[("package.json", PACKAGE_JSON)]);

    let runtime = Runtime::in_memory().unwrap();
    runtime.add_project(one.path(), None).unwrap();
    runtime.add_project(two.path(), None).unwrap();

    // Preferring the primary checkout is meant to separate `main` from a
    // worktree of one project; across projects it would silently pick one, and
    // an agent asked to restart "web" would hit the wrong repository.
    let error = runtime.resolve_service(None, "web").unwrap_err();
    match error {
        RuntimeError::InvalidInput { message } => {
            assert!(message.contains("matches several services"), "{message}");
        }
        other => panic!("expected an ambiguity error, got {other:?}"),
    }
}

#[test]
fn tool_managed_worktrees_are_not_registered() {
    let dir = repo(&[("package.json", PACKAGE_JSON)]);
    // Claude Code keeps its scratch checkouts here. They are worktrees as far
    // as git is concerned, but they are not projects a developer manages.
    let hidden = dir.path().join(".claude").join("worktrees").join("scratch");
    std::fs::create_dir_all(hidden.parent().unwrap()).unwrap();
    git(
        dir.path(),
        &[
            "worktree",
            "add",
            "-q",
            hidden.to_str().unwrap(),
            "-b",
            "claude/scratch",
        ],
    );

    let runtime = Runtime::in_memory().unwrap();
    let view = runtime.add_project(dir.path(), None).unwrap();

    assert_eq!(view.workspaces.len(), 1, "only the primary checkout");
    assert!(!view.workspaces[0].workspace.worktree);

    let _ = std::fs::remove_dir_all(&hidden);
}

#[test]
fn a_service_already_listening_on_its_port_is_reported_as_running() {
    let occupied = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = occupied.local_addr().unwrap().port();

    let config = format!(
        r#"{{ "name": "adopt", "services": {{ "web": {{ "command": "true", "port": {port} }} }} }}"#
    );
    let dir = repo(&[(".runtime.json", config.as_str())]);

    let runtime = Runtime::in_memory().unwrap();
    let view = runtime.add_project(dir.path(), None).unwrap();
    let service = view.workspaces[0].services[0].service.clone();

    let refreshed = runtime.service_view(&service).unwrap();

    // The listener here belongs to the test process, whose working directory is
    // not the project, so adoption must *not* fire: matching the port alone
    // would attribute any unrelated process to the service.
    assert_eq!(refreshed.status, runtime_types::ServiceStatus::Stopped);
    assert!(!refreshed.managed);
}

#[test]
fn correcting_an_inferred_port_is_enough_to_fix_it() {
    let dir = repo(&[("package.json", PACKAGE_JSON)]);
    let runtime = Runtime::in_memory().unwrap();
    let view = runtime.add_project(dir.path(), None).unwrap();
    let service = view.workspaces[0].services[0].service.clone();

    // Inference guessed the framework default; the project does not use it.
    assert_eq!(service.preferred_port, Some(3000));

    let updated = runtime
        .update_service(
            &service.id,
            runtime_types::ServicePatch {
                preferred_port: Some(Some(3007)),
                ..Default::default()
            },
        )
        .unwrap();

    assert_eq!(updated.preferred_port, Some(3007));
    // Untouched fields survive: correcting a port should not restate a command.
    assert_eq!(updated.command, service.command);
    assert_eq!(updated.name, service.name);
}

#[test]
fn renaming_onto_an_existing_service_is_refused() {
    let dir = repo(&[("package.json", PACKAGE_JSON)]);
    let runtime = Runtime::in_memory().unwrap();
    let view = runtime.add_project(dir.path(), None).unwrap();
    let workspace = view.workspaces[0].workspace.clone();
    let web = view.workspaces[0].services[0].service.clone();

    runtime
        .add_service(
            &workspace.id,
            runtime_types::Service {
                id: runtime_types::ServiceId::new(),
                workspace_id: workspace.id.clone(),
                name: "api".to_string(),
                service_type: Default::default(),
                command: "true".to_string(),
                cwd: workspace.path.clone(),
                env: Default::default(),
                preferred_port: None,
                health_check: None,
                auto_start: false,
                conflict_policy: Default::default(),
                depends_on: Vec::new(),
                one_shot: false,
            },
        )
        .unwrap();

    // The registry is keyed on (workspace, name); allowing this would silently
    // overwrite the other service.
    let error = runtime
        .update_service(
            &web.id,
            runtime_types::ServicePatch {
                name: Some("api".to_string()),
                ..Default::default()
            },
        )
        .unwrap_err();
    assert!(matches!(error, RuntimeError::AlreadyExists { .. }), "{error:?}");
}

#[test]
fn a_reservation_holds_the_port_before_anything_listens() {
    let dir = repo(&[(
        ".runtime.json",
        r#"{ "name": "race", "services": {
             "a": { "command": "true", "port": 39871 },
             "b": { "command": "true", "port": 39871 } } }"#,
    )]);

    let runtime = Runtime::in_memory().unwrap();
    let view = runtime.add_project(dir.path(), None).unwrap();
    let workspace = view.workspaces[0].workspace.clone();
    let services = &view.workspaces[0].services;
    let first = services[0].service.clone();
    let second = services[1].service.clone();

    let one = runtime
        .resolver()
        .reserve(&view.project, &workspace, &first, None, None, Default::default())
        .unwrap();
    assert_eq!(one.port, 39871);

    // Nothing is listening yet — the first service has only claimed the port.
    // Without leases counting as occupied, a second agent asking at exactly
    // this moment is told it is free and takes it too.
    let two = runtime
        .resolver()
        .reserve(&view.project, &workspace, &second, None, None, Default::default())
        .unwrap();
    assert_ne!(two.port, one.port, "a reservation must actually reserve");
    assert!(two.reallocated);
}

#[test]
fn exporting_produces_a_config_that_reproduces_the_registry() {
    let dir = repo(&[("package.json", PACKAGE_JSON)]);
    let runtime = Runtime::in_memory().unwrap();
    let view = runtime.add_project(dir.path(), None).unwrap();
    let service = view.workspaces[0].services[0].service.clone();

    runtime
        .update_service(
            &service.id,
            runtime_types::ServicePatch {
                preferred_port: Some(Some(3007)),
                ..Default::default()
            },
        )
        .unwrap();

    let config = runtime.export_config(&view.project.id).unwrap();
    let web = config.services.get("web").expect("web is exported");

    assert_eq!(config.name.as_deref(), Some("shop"));
    assert_eq!(web.port, Some(3007));
    assert_eq!(web.command, "pnpm run dev");
    // A corrected registry is only useful to a teammate if it round-trips
    // through the file the repository carries.
    let encoded = serde_json::to_string(&config).unwrap();
    let decoded: runtime_types::ProjectConfig = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded.services.get("web").map(|s| s.port), Some(Some(3007)));
}

/// A pnpm monorepo whose root only forwards to its packages.
fn monorepo() -> TempDir {
    let dir = repo(&[
        (
            "package.json",
            r#"{
              "name": "shop",
              "packageManager": "pnpm@9.0.0",
              "scripts": {
                "api:dev": "pnpm --filter @shop/payments dev",
                "scheduler:dev": "pnpm --filter @shop/billing dev",
                "build": "turbo run build"
              }
            }"#,
        ),
        ("pnpm-workspace.yaml", "packages:\n  - \"packages/*\"\n"),
    ]);

    for (name, package, extra) in [
        ("payments", "@shop/payments", r#""fastify": "4""#),
        ("billing", "@shop/billing", r#""zod": "3""#),
    ] {
        let member = dir.path().join("packages").join(name);
        std::fs::create_dir_all(&member).unwrap();
        std::fs::write(
            member.join("package.json"),
            format!(
                r#"{{ "name": "{package}", "scripts": {{ "dev": "tsx watch server.ts" }},
                     "dependencies": {{ {extra} }} }}"#
            ),
        )
        .unwrap();
    }
    dir
}

#[test]
fn workspace_members_become_services_rooted_in_their_own_package() {
    let dir = monorepo();
    let runtime = Runtime::in_memory().unwrap();
    let view = runtime.add_project(dir.path(), None).unwrap();

    let services = &view.workspaces[0].services;
    let mut names: Vec<&str> = services.iter().map(|s| s.service.name.as_str()).collect();
    names.sort();

    // The packages, not the root scripts that forward to them: a service rooted
    // at the repository can never match the process that runs in the package.
    assert_eq!(names, vec!["billing", "payments"]);

    let payments = services
        .iter()
        .find(|s| s.service.name == "payments")
        .unwrap();
    // Canonicalised, because macOS resolves /var to /private/var.
    assert_eq!(
        payments.service.cwd.canonicalize().unwrap(),
        dir.path()
            .join("packages")
            .join("payments")
            .canonicalize()
            .unwrap()
    );
    // Ports come from the member's own dependencies.
    assert_eq!(payments.service.preferred_port, Some(3000));
}

#[test]
fn a_root_script_that_forwards_to_a_member_is_not_duplicated() {
    let dir = monorepo();
    let runtime = Runtime::in_memory().unwrap();
    let view = runtime.add_project(dir.path(), None).unwrap();

    let services = &view.workspaces[0].services;
    // `api:dev` runs `pnpm --filter @shop/payments dev`. Keeping both it and
    // the member would start the same thing twice under two names.
    assert!(!services.iter().any(|s| s.service.name == "api"));
    assert!(!services.iter().any(|s| s.service.name == "scheduler"));
    assert_eq!(services.len(), 2);
}

#[test]
fn a_root_without_workspaces_is_unaffected() {
    let dir = repo(&[("package.json", PACKAGE_JSON)]);
    let runtime = Runtime::in_memory().unwrap();
    let view = runtime.add_project(dir.path(), None).unwrap();

    let services = &view.workspaces[0].services;
    assert_eq!(services.len(), 1);
    assert_eq!(services[0].service.name, "web");
}

#[tokio::test]
async fn a_command_that_fails_immediately_is_reported_as_a_failure() {
    // A failure both shells can produce: a script that exits non-zero, which
    // neither `>&2` nor `;` survives on Windows.
    let config = r#"{ "name": "boom", "services": {
        "web": { "command": "PY boom.py" } } }"#
        .replace("PY", if cfg!(windows) { "python" } else { "python3" });
    let script = "import sys\nprint('no such module', file=sys.stderr)\nsys.exit(1)\n";
    let dir = repo(&[(".runtime.json", config.as_str()), ("boom.py", script)]);

    let runtime = Runtime::in_memory().unwrap();
    let view = runtime.add_project(dir.path(), None).unwrap();
    let service = view.workspaces[0].services[0].service.clone();

    // Spawning is not starting. Reporting success for a process that is already
    // dead leaves the truth only in the logs, which is where this was found.
    let error = runtime
        .start_service(&service.id, Default::default())
        .await
        .unwrap_err();

    match error {
        RuntimeError::StartFailed { service, detail, .. } => {
            assert_eq!(service, "web");
            assert!(detail.contains("no such module"), "{detail}");
        }
        other => panic!("expected a start failure, got {other:?}"),
    }
}


#[tokio::test]
async fn a_service_that_starts_and_keeps_running_is_not_reported_as_failed() {
    let config = r#"{ "name": "fine", "services": { "web": { "command": "sleep 30" } } }"#;
    let dir = repo(&[(".runtime.json", config)]);

    let runtime = Runtime::in_memory().unwrap();
    let view = runtime.add_project(dir.path(), None).unwrap();
    let service = view.workspaces[0].services[0].service.clone();

    let outcome = runtime
        .start_service(&service.id, Default::default())
        .await
        .expect("a running process is a successful start");
    assert!(!outcome.reused);

    runtime
        .stop_service(&service.id, std::time::Duration::from_secs(5))
        .await
        .unwrap();
}

#[test]
fn two_services_declaring_one_port_do_not_both_claim_it() {
    let occupied = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = occupied.local_addr().unwrap().port();

    // One package, two modes — `dev` and `dev:local` — only one of which ever
    // runs. This is the shape a payments service with a demo mode has.
    let config = format!(
        r#"{{ "name": "modes", "services": {{
             "server": {{ "command": "true", "port": {port} }},
             "demo":   {{ "command": "true", "port": {port} }} }} }}"#
    );
    let dir = repo(&[(".runtime.json", config.as_str())]);

    let runtime = Runtime::in_memory().unwrap();
    let view = runtime.add_project(dir.path(), None).unwrap();

    // Whether or not the listener is attributed to this workspace, neither
    // service may claim it while the other declares the same port: at most one
    // can be running, and nothing says which.
    for service in &view.workspaces[0].services {
        let refreshed = runtime.service_view(&service.service).unwrap();
        assert!(
            !refreshed.status.is_live(),
            "{} claimed a port two services declare",
            service.service.name
        );
    }
}

#[test]
fn editing_a_service_tells_anyone_watching() {
    let dir = repo(&[("package.json", PACKAGE_JSON)]);
    let runtime = Runtime::in_memory().unwrap();
    let view = runtime.add_project(dir.path(), None).unwrap();
    let service = view.workspaces[0].services[0].service.clone();

    let mut events = runtime.events().subscribe();

    runtime
        .update_service(
            &service.id,
            runtime_types::ServicePatch {
                preferred_port: Some(Some(3007)),
                ..Default::default()
            },
        )
        .unwrap();

    // A registry edit is not a lifecycle event, but a window showing a service
    // list has to hear about it: an agent correcting a command through MCP
    // otherwise leaves the app displaying the old one indefinitely.
    let event = events.try_recv().expect("an edit is announced");
    match event {
        runtime_core::events::RuntimeEvent::ServiceChanged {
            service_id,
            removed,
            ..
        } => {
            assert_eq!(service_id, service.id);
            assert!(!removed);
        }
        other => panic!("expected a service change, got {other:?}"),
    }

    runtime.delete_service(&service.id).unwrap();
    match events.try_recv().expect("a removal is announced") {
        runtime_core::events::RuntimeEvent::ServiceChanged { removed, .. } => assert!(removed),
        other => panic!("expected a removal, got {other:?}"),
    }
}

#[test]
fn re_registering_a_project_leaves_a_curated_registry_alone() {
    let dir = repo(&[("package.json", PACKAGE_JSON)]);
    let runtime = Runtime::in_memory().unwrap();
    let view = runtime.add_project(dir.path(), None).unwrap();
    let service = view.workspaces[0].services[0].service.clone();

    runtime
        .update_service(
            &service.id,
            runtime_types::ServicePatch {
                command: Some("pnpm run dev:local".to_string()),
                ..Default::default()
            },
        )
        .unwrap();

    // Adding the same directory again happens easily — the Discover tab, a
    // scan, or `project add` run twice. Inference must not undo curation.
    runtime.add_project(dir.path(), None).unwrap();

    let after = runtime.get_project(&view.project.id).unwrap();
    assert_eq!(after.total_services, 1);
    assert_eq!(
        after.workspaces[0].services[0].service.command,
        "pnpm run dev:local",
        "a corrected command was overwritten by the guess it replaced"
    );
}

#[test]
fn a_deleted_service_stays_deleted() {
    let dir = repo(&[("package.json", PACKAGE_JSON)]);
    let runtime = Runtime::in_memory().unwrap();
    let view = runtime.add_project(dir.path(), None).unwrap();
    let service = view.workspaces[0].services[0].service.clone();

    runtime.delete_service(&service.id).unwrap();
    runtime.add_project(dir.path(), None).unwrap();

    // Removing a service detection invented, only to have it reappear, is the
    // shape of a tool that does not believe the user.
    let after = runtime.get_project(&view.project.id).unwrap();
    assert_eq!(after.total_services, 0);
}

/// The daemon dying must not take the services with it.
///
/// Output used to go through a pipe held by the daemon. When the daemon died
/// the read end closed, and the next thing a service printed killed it with
/// SIGPIPE — so capturing logs quietly made every service's life depend on the
/// daemon's, which is the one thing the daemon is not supposed to be for.
#[tokio::test]
async fn a_service_writing_output_does_not_depend_on_a_reader() {
    let logs = tempfile::tempdir().unwrap();
    // Long-lived on purpose: `start_service` spends its own grace period
    // watching for an immediate failure, so a short command would simply have
    // finished by the time this asserts anything.
    let config = r#"{ "name": "chatty", "services": {
        "loop": { "command": "PY -u loop.py" } } }"#
        .replace("PY", if cfg!(windows) { "python" } else { "python3" });
    // A file rather than a `-c` one-liner. The command has to survive a JSON
    // string, a Rust literal and whichever shell the platform uses, and each
    // of those has its own opinion about quotes and newlines — a script has
    // none of that in its way.
    let script = "import time\nwhile True:\n    print('tick', flush=True)\n    time.sleep(0.2)\n";
    let dir = repo(&[(".runtime.json", config.as_str()), ("loop.py", script)]);

    let runtime = Runtime::in_memory_with_logs(logs.path()).unwrap();
    let view = runtime.add_project(dir.path(), None).unwrap();
    let service = view.workspaces[0].services[0].service.clone();

    runtime
        .start_service(&service.id, Default::default())
        .await
        .unwrap();
    let pid = runtime
        .service_view(&service)
        .unwrap()
        .instance
        .expect("an instance")
        .pid;

    // Drop everything the runtime holds — the tailing stacks, the child handle,
    // the log store. This is what a killed daemon leaves behind.
    drop(runtime);
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;

    // Asked of the adapter rather than of `ps`, which does not exist on every
    // platform — and `.unwrap_or(false)` on a missing command reports the
    // process as dead, which is this test's failure condition. It would have
    // passed for the wrong reason just as easily as it failed for one.
    let adapter = runtime_core::platform::current();
    let alive = adapter
        .process()
        .process_info(pid)
        .ok()
        .flatten()
        .is_some();
    assert!(alive, "the service died when nothing was reading its output");

    if let Some(info) = adapter.process().process_info(pid).ok().flatten() {
        let identity = runtime_adapter::ProcessIdentity::new(info.pid, info.start_time_ms);
        let _ = adapter.process().terminate_tree(
            &identity,
            runtime_adapter::TerminationMode::Forceful,
        );
    }
}


/// A `.runtime.json` has to survive being written and read back.
///
/// Every field that only exists on one side of that trip is a field the
/// documentation promises and the tool silently drops — which is worse than
/// not supporting it, because the file looks like it worked.
#[tokio::test]
async fn exported_config_carries_ordering_back() {
    let dir = repo(&[("package.json", "{}")]);
    let runtime = Runtime::in_memory().unwrap();
    let project = runtime.add_project(dir.path(), None).unwrap();
    let workspace = runtime
        .store()
        .list_workspaces(&project.project.id)
        .unwrap()
        .remove(0);

    for (name, depends_on, one_shot) in [
        ("migrate", vec![], true),
        ("api", vec!["migrate".to_string()], false),
    ] {
        let service = runtime_types::Service {
            id: runtime_types::ServiceId::new(),
            workspace_id: workspace.id.clone(),
            name: name.to_string(),
            service_type: ServiceType::Custom,
            command: "sleep 1".to_string(),
            cwd: dir.path().to_path_buf(),
            env: Default::default(),
            preferred_port: None,
            health_check: None,
            auto_start: false,
            conflict_policy: ConflictPolicy::Fail,
            depends_on,
            one_shot,
        };
        runtime.add_service(&workspace.id, service).unwrap();
    }

    let config = runtime.export_config(&project.project.id).unwrap();
    let raw = serde_json::to_string_pretty(&config).unwrap();
    std::fs::write(dir.path().join(".runtime.json"), raw).unwrap();

    // Forget everything and read the checkout again.
    runtime.remove_project(&project.project.id).unwrap();
    let reread = runtime.add_project(dir.path(), None).unwrap();
    let workspace = runtime
        .store()
        .list_workspaces(&reread.project.id)
        .unwrap()
        .remove(0);
    let services = runtime.store().list_services(&workspace.id).unwrap();

    let migrate = services.iter().find(|s| s.name == "migrate").expect("migrate");
    assert!(migrate.one_shot, "one_shot did not survive the round trip");

    let api = services.iter().find(|s| s.name == "api").expect("api");
    assert_eq!(
        api.depends_on,
        vec!["migrate".to_string()],
        "depends_on did not survive the round trip"
    );
}


/// Registering a worktree is how a checkout gets what it needs to run.
///
/// It has to work when the worktree already exists — which is the ordinary
/// case, since adding a project registers every worktree it finds, at a moment
/// when the project may have no services at all.
#[tokio::test]
async fn registering_a_worktree_tops_up_its_services() {
    let dir = repo(&[("package.json", "{}")]);
    let worktree = tempfile::tempdir().unwrap();
    let path = worktree.path().join("feature");
    git(dir.path(), &["worktree", "add", "-q", "-b", "feature", path.to_str().unwrap()]);

    let runtime = Runtime::in_memory().unwrap();
    // Adding the project registers the worktree, before any service exists.
    let project = runtime.add_project(dir.path(), None).unwrap();
    let primary = runtime
        .store()
        .list_workspaces(&project.project.id)
        .unwrap()
        .into_iter()
        .find(|w| !w.worktree)
        .unwrap();

    let service = runtime_types::Service {
        id: runtime_types::ServiceId::new(),
        workspace_id: primary.id.clone(),
        name: "web".to_string(),
        service_type: ServiceType::Web,
        command: "sleep 1".to_string(),
        cwd: dir.path().to_path_buf(),
        env: Default::default(),
        preferred_port: Some(4100),
        health_check: None,
        auto_start: false,
        conflict_policy: ConflictPolicy::Fail,
        depends_on: Vec::new(),
        one_shot: false,
    };
    runtime.add_service(&primary.id, service).unwrap();

    runtime.register_worktree(&project.project.id, &path).unwrap();

    let branch = runtime
        .store()
        .list_workspaces(&project.project.id)
        .unwrap()
        .into_iter()
        .find(|w| w.worktree)
        .expect("the worktree should be registered");
    let copied = runtime.store().list_services(&branch.id).unwrap();
    assert_eq!(copied.len(), 1, "the worktree got no services");
    // Compared canonically: the runtime stores the resolved path, and on macOS
    // a temp directory is `/var/...` on the way in and `/private/var/...` on
    // the way out.
    assert_eq!(
        std::fs::canonicalize(&copied[0].cwd).unwrap(),
        std::fs::canonicalize(&path).unwrap(),
        "the copy still points at the primary checkout"
    );

    // Again, with the copy edited: topping up must not undo that.
    let mut edited = copied.into_iter().next().unwrap();
    edited.command = "sleep 2".to_string();
    runtime.store().upsert_service(&edited).unwrap();
    runtime.register_worktree(&project.project.id, &path).unwrap();

    let after = runtime.store().list_services(&branch.id).unwrap();
    assert_eq!(after.len(), 1, "registering again duplicated a service");
    assert_eq!(after[0].command, "sleep 2", "an edited copy was overwritten");
}


/// Wait until a line has been logged at least `want` times, or give up.
///
/// These tests are about a line appearing *twice*, and a fixed sleep cannot
/// tell "not logged twice" from "not logged yet" — on a loaded machine the
/// second reading is the common one, and the test then reports duplication
/// that did not happen. Polling to the point where the count could be right
/// removes that reading: slowness costs time here, never a verdict.
async fn logged_at_least(runtime: &Runtime, id: &ServiceId, line: &str, want: usize) -> usize {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let seen = runtime
            .read_logs(id, 200, None)
            .unwrap()
            .iter()
            .filter(|entry| entry.message == line)
            .count();
        if seen >= want || std::time::Instant::now() > deadline {
            return seen;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// Restarting a service must not log its output twice.
///
/// The pumps that follow a capture file are deliberately left running when a
/// service exits, so the last thing it printed — usually the reason — still
/// arrives. `finish` took them out of the supervisor without stopping them,
/// which made them unreachable while they went on reading the same file the
/// next run appends to. Two runs, two readers, every line twice; three runs,
/// three times. It reads as a service repeating itself.
#[tokio::test]
async fn restarting_does_not_log_the_same_line_twice() {
    let logs = tempfile::tempdir().unwrap();
    let config = r#"{ "name": "chatty", "services": {
        "once": { "command": "PY once.py" } } }"#
        .replace("PY", if cfg!(windows) { "python" } else { "python3" });
    let script = "import sys\nprint('LINE', file=sys.stderr)\nsys.exit(1)\n";
    let dir = repo(&[(".runtime.json", config.as_str()), ("once.py", script)]);

    let runtime = Runtime::in_memory_with_logs(logs.path()).unwrap();
    let view = runtime.add_project(dir.path(), None).unwrap();
    let service = view.workspaces[0].services[0].service.clone();

    for run in 1..=3 {
        let _ = runtime.start_service(&service.id, Default::default()).await;
        let seen = logged_at_least(&runtime, &service.id, "LINE", run).await;
        assert_eq!(seen, run, "after {run} runs the line was logged {seen} times");
    }
}


/// The same must hold for a service that is stopped rather than one that exits.
///
/// A run has two endings, and the first fix for the doubled lines only covered
/// one of them. `stop` still took the readers out of the supervisor and left
/// them reading, on the belief that closing the pipes would end them by itself.
/// It does not, and a service that is restarted rather than left to fall over
/// is the common case — so the duplicate lines came straight back.
#[tokio::test]
async fn stopping_does_not_log_the_same_line_twice() {
    let logs = tempfile::tempdir().unwrap();
    let config = r#"{ "name": "chatty", "services": {
        "stays": { "command": "PY stays.py" } } }"#
        .replace("PY", if cfg!(windows) { "python" } else { "python3" });
    // Prints once, then stays up until it is stopped.
    let script = "import sys, time\nprint('LINE', flush=True)\ntime.sleep(600)\n";
    let dir = repo(&[(".runtime.json", config.as_str()), ("stays.py", script)]);

    let runtime = Runtime::in_memory_with_logs(logs.path()).unwrap();
    let view = runtime.add_project(dir.path(), None).unwrap();
    let service = view.workspaces[0].services[0].service.clone();

    for run in 1..=3 {
        runtime
            .start_service(&service.id, Default::default())
            .await
            .unwrap();
        // Its one line has to have been captured before stopping proves
        // anything about what stopping does to the readers.
        logged_at_least(&runtime, &service.id, "LINE", run).await;
        runtime
            .stop_service(&service.id, std::time::Duration::from_secs(5))
            .await
            .unwrap();

        // `stop_service` drains before it returns, so by here this run's
        // readers are gone and a duplicate would already have been written.
        let seen = logged_at_least(&runtime, &service.id, "LINE", run).await;
        assert_eq!(seen, run, "after {run} stopped runs the line was logged {seen} times");
    }
}


/// Polling with a cursor must not be answered with a line that has no place in
/// the sequence.
///
/// A service the runtime did not start has no captured output, and saying so is
/// useful — once, to a reader starting from the beginning. Returning it to a
/// cursored read means "nothing new" is answered with a line whose `seq` is 0,
/// so a caller that advances its cursor from the reply is sent backwards and
/// asks again for everything it has already seen. The log repeats, and it reads
/// as the service repeating itself.
#[tokio::test]
async fn a_cursored_read_is_not_answered_with_a_synthesised_line() {
    let dir = repo(&[("package.json", "{}")]);
    let runtime = Runtime::in_memory().unwrap();
    let project = runtime.add_project(dir.path(), None).unwrap();
    let workspace = runtime
        .store()
        .list_workspaces(&project.project.id)
        .unwrap()
        .remove(0);

    let service = runtime_types::Service {
        id: runtime_types::ServiceId::new(),
        workspace_id: workspace.id.clone(),
        name: "quiet".to_string(),
        service_type: ServiceType::Web,
        command: "sleep 1".to_string(),
        cwd: dir.path().to_path_buf(),
        env: Default::default(),
        preferred_port: None,
        health_check: None,
        auto_start: false,
        conflict_policy: ConflictPolicy::Fail,
        depends_on: Vec::new(),
        one_shot: false,
    };
    let service = runtime.add_service(&workspace.id, service).unwrap();

    // Whatever a reader starting from the beginning is told, a cursored read
    // that finds nothing new must be told nothing at all.
    let _ = runtime.read_logs(&service.id, 100, None).unwrap();
    let following = runtime.read_logs(&service.id, 100, Some(0)).unwrap();
    assert!(
        following.is_empty(),
        "a cursored read was answered with {following:?}"
    );
}

/// Taking over is refused for a service nothing else is holding up.
///
/// The rule the whole design rests on is that the runtime does not terminate
/// what it did not start. Take-over is the one door through it, so what it
/// refuses matters more than what it does: a stopped service has nothing to
/// take over and is simply started, and a service under another supervisor is
/// left alone, because stopping it here is undone a second later by whatever
/// is watching it.
#[tokio::test]
async fn taking_over_something_stopped_just_starts_it() {
    let dir = repo(&[(
        ".runtime.json",
        r#"{ "name": "app", "services": { "web": { "command": "CMD" } } }"#
            .replace("CMD", if cfg!(windows) { "ping -n 30 127.0.0.1" } else { "sleep 30" })
            .as_str(),
    )]);
    let runtime = Runtime::in_memory().unwrap();
    let view = runtime.add_project(dir.path(), None).unwrap();
    let service = view.workspaces[0].services[0].service.clone();

    let taken = runtime
        .take_over(&service.id, std::time::Duration::from_secs(5))
        .await
        .unwrap();
    assert!(taken.status.is_live(), "{taken:?}");
    assert!(taken.managed, "it should be ours now");

    // And again, on something already ours: nothing to take over.
    let again = runtime
        .take_over(&service.id, std::time::Duration::from_secs(5))
        .await
        .unwrap();
    assert!(again.managed);

    runtime
        .stop_service(&service.id, std::time::Duration::from_secs(5))
        .await
        .unwrap();
}

/// Two clones of one repository are one project with two checkouts.
///
/// Registering them separately produced two entries with the same name, and a
/// selector naming that name picked one of them by luck. The runtime already
/// models a project as one thing with several checkouts told apart by branch;
/// a second clone is that situation reached a different way.
#[tokio::test]
async fn a_second_clone_of_a_repository_becomes_a_checkout_of_it() {
    let remote = "git@github.com:someone/shop.git";
    let first = repo(&[("package.json", PACKAGE_JSON)]);
    let second = repo(&[("package.json", PACKAGE_JSON)]);
    for dir in [&first, &second] {
        git(dir.path(), &["remote", "add", "origin", remote]);
    }
    git(second.path(), &["checkout", "-b", "other"]);

    let runtime = Runtime::in_memory().unwrap();
    let one = runtime.add_project(first.path(), None).unwrap();
    let two = runtime.add_project(second.path(), None).unwrap();

    assert_eq!(one.project.id, two.project.id, "a second project was made");
    assert_eq!(runtime.list_projects().unwrap().len(), 1);
    assert_eq!(two.workspaces.len(), 2, "{:?}", two.workspaces);

    // And the same branch twice is worth saying out loud.
    let third = repo(&[("package.json", PACKAGE_JSON)]);
    git(third.path(), &["remote", "add", "origin", remote]);
    git(third.path(), &["checkout", "-b", "other"]);
    runtime.add_project(third.path(), None).unwrap();

    let findings = runtime.diagnose().unwrap();
    assert!(
        findings.iter().any(|f| f.message.contains("checkouts are on this branch")),
        "{findings:?}"
    );
}

/// With a repository cloned twice, a service name must not pick one by luck.
///
/// The resolver preferred "the checkout that is not a linked worktree", which
/// assumed a project had exactly one of those. A second clone registered as a
/// checkout is not a worktree either, so the assumption broke the moment that
/// became possible, and an edit meant for one clone landed in the other —
/// silently, which is the part that matters.
#[tokio::test]
async fn a_path_selector_names_the_checkout_it_points_inside() {
    let remote = "git@github.com:someone/shop.git";
    let first = repo(&[("package.json", PACKAGE_JSON)]);
    let second = repo(&[("package.json", PACKAGE_JSON)]);
    for dir in [&first, &second] {
        git(dir.path(), &["remote", "add", "origin", remote]);
    }
    git(second.path(), &["checkout", "-b", "other"]);

    let runtime = Runtime::in_memory().unwrap();
    let view = runtime.add_project(first.path(), None).unwrap();
    runtime.add_project(second.path(), None).unwrap();
    let project = view.project.clone();

    // A bare name answers with the project's own root. This one passes under
    // the old rule too, since the root also happens to be the first checkout
    // registered — it is here as a smoke check, not as the guard. The guard is
    // below: without narrowing, that assertion fails.
    let root = runtime
        .store()
        .find_workspace_by_path(&canonical(first.path()))
        .unwrap()
        .unwrap();
    let picked = runtime.resolve_service(Some(&project), "web").unwrap();
    assert_eq!(picked.workspace_id, root.id);

    // A path inside the other clone names that one.
    let other = runtime
        .workspace_for_selector(second.path().to_str().unwrap())
        .unwrap()
        .expect("the second checkout should be found by its path");
    let picked = runtime
        .resolve_service_in(Some(&project), Some(&other), "web")
        .unwrap();
    assert_eq!(picked.workspace_id, other.id, "the path did not narrow anything");
}

fn canonical(path: &std::path::Path) -> std::path::PathBuf {
    std::fs::canonicalize(path).unwrap()
}

/// When two checkouts share a branch, saying so must say which is which.
///
/// The list of candidates was built from project, branch and service name —
/// which is precisely what is the same about them — so it printed one string
/// twice and left the reader no way to choose. The way out is a path, so the
/// paths are what it shows.
#[tokio::test]
async fn an_ambiguous_name_says_how_to_tell_the_candidates_apart() {
    let remote = "git@github.com:someone/shop.git";
    let first = repo(&[("package.json", PACKAGE_JSON)]);
    let second = repo(&[("package.json", PACKAGE_JSON)]);
    for dir in [&first, &second] {
        git(dir.path(), &["remote", "add", "origin", remote]);
        git(dir.path(), &["checkout", "-B", "shared"]);
    }

    let runtime = Runtime::in_memory().unwrap();
    let view = runtime.add_project(first.path(), None).unwrap();
    runtime.add_project(second.path(), None).unwrap();

    let error = runtime
        .resolve_service(Some(&view.project), "shared/web")
        .unwrap_err()
        .to_string();
    for dir in [&first, &second] {
        let path = canonical(dir.path());
        assert!(
            error.contains(&path.display().to_string()),
            "the error does not name {}: {error}",
            path.display()
        );
    }

    // And the finding says the same thing the error does, rather than claiming
    // a port conflict — the checkouts have different offsets, so their ports
    // do not collide at all.
    let findings = runtime.diagnose().unwrap();
    let branch = findings
        .iter()
        .find(|f| f.message.contains("checkouts are on this branch"))
        .expect("no finding about the shared branch");
    assert!(!branch.message.contains("ports"), "{}", branch.message);
}
