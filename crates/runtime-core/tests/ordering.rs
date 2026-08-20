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

#[tokio::test]
async fn starting_a_one_shot_directly_runs_it_rather_than_supervising_it() {
    // The Run button on a migration goes through the same call as Start on a
    // server. Without this, the ordinary path spawns it, watches it exit the
    // moment it succeeded, and records that success as a failure.
    let dir = repo();
    let runtime = Runtime::in_memory().unwrap();
    let project = runtime.add_project(dir.path(), None).unwrap();
    let workspace = runtime
        .store()
        .list_workspaces(&project.project.id)
        .unwrap()
        .remove(0);

    let marker = dir.path().join("seeded");
    let seed = declare(
        &runtime,
        &workspace.id,
        dir.path(),
        "seed",
        &format!("touch {}", marker.display()),
        &[],
        true,
    );

    runtime
        .start_service(&seed.id, Default::default())
        .await
        .expect("a one-shot that succeeds must not be reported as failed");

    assert!(marker.exists());
    let view = runtime
        .service_view(&runtime.require_service(&seed.id).unwrap())
        .unwrap();
    assert_ne!(
        view.status,
        runtime_types::ServiceStatus::Failed,
        "a step that finished is not a failure"
    );
}

/// A service the runtime did not start, holding the port it declares.
async fn adopted(dir: &TempDir, runtime: &Runtime) -> (runtime_types::Service, std::process::Child) {
    let project = runtime.add_project(dir.path(), None).unwrap();
    let workspace = runtime
        .store()
        .list_workspaces(&project.project.id)
        .unwrap()
        .remove(0);

    // A real listener, started outside the runtime, in the checkout.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let child = Command::new("python3")
        .args(["-m", "http.server", &port.to_string()])
        .current_dir(dir.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("python3 must be installed");
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;

    let mut service = declare(runtime, &workspace.id, dir.path(), "web", "sleep 30", &[], false);
    service.preferred_port = Some(port);
    runtime.store().upsert_service(&service).unwrap();
    (service, child)
}

#[tokio::test]
async fn starting_something_already_serving_returns_it_rather_than_a_second_copy() {
    // This is the shape of real damage: the duplicate arrives with different
    // arguments than the process already serving, and for a Next project that
    // means a development build written over the production one.
    let dir = repo();
    let runtime = Runtime::in_memory().unwrap();
    let (service, mut child) = adopted(&dir, &runtime).await;

    let outcome = runtime
        .start_service(&service.id, Default::default())
        .await
        .expect("starting something already up is not an error");

    assert!(outcome.reused, "a second copy was started");
    assert!(
        runtime.store().latest_instance(&service.id).unwrap().is_none(),
        "the runtime recorded an instance it did not start"
    );
    let _ = child.kill();
}

#[tokio::test]
async fn restarting_something_the_runtime_did_not_start_is_refused() {
    let dir = repo();
    let runtime = Runtime::in_memory().unwrap();
    let (service, mut child) = adopted(&dir, &runtime).await;

    let error = runtime
        .restart_service(&service.id, Default::default())
        .await
        .expect_err("restart means stop, and this is not the runtime's to stop");
    let text = error.to_string();
    assert!(text.contains("not started by the runtime"), "{text}");
    let _ = child.kill();
}

#[tokio::test]
async fn a_dependency_naming_nothing_is_found_before_it_is_needed() {
    // Otherwise it is found halfway through a start, with everything before it
    // already up.
    let dir = repo();
    let runtime = Runtime::in_memory().unwrap();
    let project = runtime.add_project(dir.path(), None).unwrap();
    let workspace = runtime
        .store()
        .list_workspaces(&project.project.id)
        .unwrap()
        .remove(0);

    declare(&runtime, &workspace.id, dir.path(), "api", "sleep 30", &["db"], false);

    let findings = runtime.diagnose().unwrap();
    let found = findings
        .iter()
        .find(|f| f.message.contains("'db'"))
        .expect("a dependency that names nothing is a finding");
    assert!(found.certain);
    assert!(found.subject.ends_with("/api"), "{}", found.subject);
}

#[tokio::test]
async fn services_that_depend_on_each_other_are_found() {
    let dir = repo();
    let runtime = Runtime::in_memory().unwrap();
    let project = runtime.add_project(dir.path(), None).unwrap();
    let workspace = runtime
        .store()
        .list_workspaces(&project.project.id)
        .unwrap()
        .remove(0);

    declare(&runtime, &workspace.id, dir.path(), "a", "sleep 30", &["b"], false);
    declare(&runtime, &workspace.id, dir.path(), "b", "sleep 30", &["a"], false);

    let findings = runtime.diagnose().unwrap();
    assert!(
        findings.iter().any(|f| f.message.contains("depend on each other")),
        "{findings:?}"
    );
}

#[tokio::test]
async fn a_task_step_that_was_removed_is_found() {
    let dir = repo();
    let runtime = Runtime::in_memory().unwrap();
    let project = runtime.add_project(dir.path(), None).unwrap();
    let workspace = runtime
        .store()
        .list_workspaces(&project.project.id)
        .unwrap()
        .remove(0);

    let web = declare(&runtime, &workspace.id, dir.path(), "web", "sleep 30", &[], false);
    runtime
        .set_task(&workspace.id, "dev", vec!["web".to_string()])
        .unwrap();
    // Steps are checked when a task is declared; a service can go afterwards.
    runtime.delete_service(&web.id).unwrap();

    let findings = runtime.diagnose().unwrap();
    assert!(
        findings.iter().any(|f| f.message.contains("no longer a service")),
        "{findings:?}"
    );
}

#[tokio::test]
async fn a_healthy_checkout_reports_nothing() {
    let dir = repo();
    let runtime = Runtime::in_memory().unwrap();
    let project = runtime.add_project(dir.path(), None).unwrap();
    let workspace = runtime
        .store()
        .list_workspaces(&project.project.id)
        .unwrap()
        .remove(0);

    let db = declare(&runtime, &workspace.id, dir.path(), "db", "sleep 30", &[], false);
    declare(&runtime, &workspace.id, dir.path(), "api", "sleep 30", &["db"], false);
    let _ = db;

    assert!(runtime.diagnose().unwrap().is_empty(), "quiet is the point");
}
