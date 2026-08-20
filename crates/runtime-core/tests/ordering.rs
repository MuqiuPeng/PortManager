//! Starting things in the order they need to start in.
//!
//! The cases worth an integration test are the ones the unit tests cannot
//! reach: that a one-shot step actually runs and its failure stops what comes
//! after it, and that a service already up is left alone rather than restarted.

use std::path::Path;
use std::process::Command;

use runtime_core::Runtime;
use runtime_types::{Service, ServiceId, ServiceType};
use tempfile::TempDir;

fn repo() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), "{}").unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "t@example.com"],
        vec!["config", "user.name", "t"],
        vec!["add", "-A"],
        vec!["commit", "-qm", "init"],
    ] {
        let status = Command::new("git")
            .args(&args)
            .current_dir(dir.path())
            .status()
            .expect("git must be installed");
        assert!(status.success());
    }
    dir
}

fn declare(
    runtime: &Runtime,
    workspace: &runtime_types::WorkspaceId,
    cwd: &Path,
    name: &str,
    command: &str,
    depends_on: &[&str],
    one_shot: bool,
) -> Service {
    let service = Service {
        id: ServiceId::new(),
        workspace_id: workspace.clone(),
        name: name.to_string(),
        service_type: ServiceType::Custom,
        command: command.to_string(),
        cwd: cwd.to_path_buf(),
        env: Default::default(),
        preferred_port: None,
        health_check: None,
        auto_start: false,
        conflict_policy: runtime_types::ConflictPolicy::Reuse,
        depends_on: depends_on.iter().map(|d| d.to_string()).collect(),
        one_shot,
    };
    runtime.add_service(workspace, service).unwrap()
}

#[tokio::test]
async fn a_one_shot_step_runs_before_what_depends_on_it() {
    let dir = repo();
    let runtime = Runtime::in_memory().unwrap();
    let project = runtime.add_project(dir.path(), None).unwrap();
    let workspace = runtime
        .store()
        .list_workspaces(&project.project.id)
        .unwrap()
        .remove(0);

    let marker = dir.path().join("migrated");
    declare(
        &runtime,
        &workspace.id,
        dir.path(),
        "migrate",
        &format!("touch {}", marker.display()),
        &[],
        true,
    );
    let api = declare(
        &runtime,
        &workspace.id,
        dir.path(),
        "api",
        "sleep 30",
        &["migrate"],
        false,
    );

    runtime
        .start_service(&api.id, Default::default())
        .await
        .unwrap();

    assert!(marker.exists(), "the migration did not run");
    let _ = runtime.stop_service(&api.id, std::time::Duration::from_secs(5)).await;
}

#[tokio::test]
async fn a_failing_one_shot_stops_what_would_have_followed() {
    // The reason one-shots are a separate kind: starting an API against a
    // database whose migration failed is worse than not starting it.
    let dir = repo();
    let runtime = Runtime::in_memory().unwrap();
    let project = runtime.add_project(dir.path(), None).unwrap();
    let workspace = runtime
        .store()
        .list_workspaces(&project.project.id)
        .unwrap()
        .remove(0);

    declare(
        &runtime,
        &workspace.id,
        dir.path(),
        "migrate",
        "echo 'relation does not exist' >&2; exit 1",
        &[],
        true,
    );
    let api = declare(
        &runtime,
        &workspace.id,
        dir.path(),
        "api",
        "sleep 30",
        &["migrate"],
        false,
    );

    let error = runtime
        .start_service(&api.id, Default::default())
        .await
        .expect_err("a failed migration must stop the start");
    let text = error.to_string();
    assert!(text.contains("migrate"), "{text}");
    assert!(text.contains("relation does not exist"), "{text}");

    // And nothing was started behind it.
    let view = runtime.service_view(&runtime.require_service(&api.id).unwrap()).unwrap();
    assert!(!view.status.is_live(), "api started despite the failure");
}

#[tokio::test]
async fn a_dependency_already_running_is_not_restarted() {
    let dir = repo();
    let runtime = Runtime::in_memory().unwrap();
    let project = runtime.add_project(dir.path(), None).unwrap();
    let workspace = runtime
        .store()
        .list_workspaces(&project.project.id)
        .unwrap()
        .remove(0);

    let db = declare(&runtime, &workspace.id, dir.path(), "db", "sleep 30", &[], false);
    let api = declare(
        &runtime,
        &workspace.id,
        dir.path(),
        "api",
        "sleep 30",
        &["db"],
        false,
    );

    runtime.start_service(&db.id, Default::default()).await.unwrap();
    let first = runtime
        .store()
        .latest_instance(&db.id)
        .unwrap()
        .expect("db should be running");

    runtime.start_service(&api.id, Default::default()).await.unwrap();

    let after = runtime
        .store()
        .latest_instance(&db.id)
        .unwrap()
        .expect("db should still be running");
    assert_eq!(
        first.pid, after.pid,
        "the dependency was restarted instead of being left alone"
    );

    let _ = runtime.stop_service(&api.id, std::time::Duration::from_secs(5)).await;
    let _ = runtime.stop_service(&db.id, std::time::Duration::from_secs(5)).await;
}
