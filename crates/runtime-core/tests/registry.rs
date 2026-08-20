//! Integration tests for the parts of the core that are easy to get subtly
//! wrong: what gets inferred, how worktrees are numbered, and what the port
//! allocator does when it cannot have what it asked for.

use std::net::TcpListener;
use std::path::Path;
use std::process::Command;

use runtime_core::Runtime;
use runtime_types::{ConflictPolicy, RuntimeError, ServiceType};
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
        payments.service.cwd,
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
    let config = r#"{ "name": "boom", "services": {
        "web": { "command": "echo 'no such module' >&2; exit 1" } } }"#;
    let dir = repo(&[(".runtime.json", config)]);

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
        "loop": { "command": "while true; do echo tick; sleep 0.2; done" } } }"#;
    let dir = repo(&[(".runtime.json", config)]);

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

    // Drop everything the runtime holds — the tailing tasks, the child handle,
    // the log store. This is what a killed daemon leaves behind.
    drop(runtime);
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;

    let alive = std::process::Command::new("ps")
        .args(["-p", &pid.to_string()])
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false);
    assert!(alive, "the service died when nothing was reading its output");

    let _ = std::process::Command::new("kill").arg(pid.to_string()).status();
}
