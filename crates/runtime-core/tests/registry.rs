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
