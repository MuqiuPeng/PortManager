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

/// A command that stays up until something stops it.
///
/// The sample commands are the only platform-specific thing in these tests, and
/// gating the tests instead would give up exactly what the Windows run is for:
/// the spawn path, which is where the platforms actually differ.
fn stays_up() -> &'static str {
    // Long enough that it cannot expire mid-suite. It used to be thirty
    // seconds, which was fine until the suite grew past that: a stand-in for
    // "still running" that stops running on its own reports whatever the test
    // was about as broken, and the failure names the assertion rather than the
    // clock. Every test kills these itself, so the number only has to be
    // bigger than the slowest run.
    if cfg!(windows) {
        // `ping` sends one a second, so the count is the service's lifetime.
        // Sixty was not enough, and neither was six hundred: these tests run in
        // parallel, each spawning services and resolving every listening socket
        // on the machine, and on Windows that takes minutes — so the service
        // exited on its own part way through and the assertions read a dead
        // dependency as one that had been restarted. Both platforms wait the
        // same twenty minutes now, because the number only has to outlast the
        // slowest run and there is no reason for them to differ.
        "ping -n 1200 127.0.0.1"
    } else {
        "sleep 1200"
    }
}

/// A command that creates a file and succeeds.
///
/// Through the interpreter these tests already need, rather than each
/// platform's own idiom: `touch` does not exist on Windows and `type nul >`
/// depends on which shell the runtime chose.
fn creates(path: &Path) -> String {
    // No quotes of its own. The command goes through a shell — `sh -c` or
    // `cmd /C` — and each keeps a different set, so the one that survives both
    // is the one that needs none. `pathlib.Path(...).touch()` cannot be
    // written without quoting the path, so the path arrives as an argument.
    format!("{} -c \"import sys,pathlib;pathlib.Path(sys.argv[1]).touch()\" {}", python(), path.display())
}

/// `python3` on Unix, `python` on Windows, where the 3 is not part of the name.
fn python() -> &'static str {
    if cfg!(windows) {
        "python"
    } else {
        "python3"
    }
}

/// A command that says something on stderr and fails.
fn fails_loudly() -> &'static str {
    if cfg!(windows) {
        "echo relation does not exist 1>&2 & exit 1"
    } else {
        "echo 'relation does not exist' >&2; exit 1"
    }
}

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
        &creates(&marker),
        &[],
        true,
    );
    let api = declare(
        &runtime,
        &workspace.id,
        dir.path(),
        "api",
        stays_up(),
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
        fails_loudly(),
        &[],
        true,
    );
    let api = declare(
        &runtime,
        &workspace.id,
        dir.path(),
        "api",
        stays_up(),
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

    let db = declare(&runtime, &workspace.id, dir.path(), "db", stays_up(), &[], false);
    let api = declare(
        &runtime,
        &workspace.id,
        dir.path(),
        "api",
        stays_up(),
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
        &creates(&marker),
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

/// Whether this platform can attribute a running process to a checkout.
///
/// Adoption needs the port's owner resolved to a working directory, which the
/// Windows adapter does not report yet — see `docs/windows.md`. Reported rather
/// than gated on the platform, so these start running there the moment it does,
/// without anybody remembering to come back and remove a `cfg`.
fn adoption_is_possible(runtime: &Runtime, service: &runtime_types::Service) -> bool {
    let possible = runtime
        .service_view(service)
        .map(|view| view.status.is_live())
        .unwrap_or(false);
    if !possible {
        eprintln!(
            "skipping: this platform did not attribute the listener to its checkout, \
             so there is nothing to adopt (see docs/windows.md, process metadata)"
        );
    }
    possible
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
    // Kept, rather than sent to /dev/null: when this does not come up the only
    // thing that can say why is the process itself, and throwing that away is
    // how "the listener never came up" became the whole of what was known.
    let log = dir.path().join("listener.log");
    // `python3` on Unix, `python` on Windows, where the 3 is not part of the
    // name. Tried in turn rather than assumed, since the failure of the wrong
    // guess is a test that says only "the listener never came up".
    let mut child = ["python3", "python"]
        .iter()
        .find_map(|program| {
            Command::new(program)
                .args(["-m", "http.server", &port.to_string(), "--bind", "127.0.0.1"])
                .current_dir(dir.path())
                .stdout(std::process::Stdio::null())
                .stderr(std::fs::File::create(&log).unwrap())
                .spawn()
                .ok()
        })
        .expect("python must be installed to run these tests");

    // Waited for rather than slept through. A fixed pause is a guess about how
    // fast the machine is, and it was wrong the first time this ran anywhere
    // but the laptop it was written on: the listener was not up, so nothing
    // was adopted, so the test saw exactly the behaviour it exists to forbid.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            break;
        }
        if let Ok(Some(status)) = child.try_wait() {
            panic!(
                "the listener exited with {status} before binding {port}: {}",
                std::fs::read_to_string(&log).unwrap_or_default()
            );
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the listener never bound {port} in time: {}",
            std::fs::read_to_string(&log).unwrap_or_default()
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    // And then past the port table's cache, which is what the fixed pause this
    // replaced was accidentally covering: the runtime reuses one scan for a
    // moment, so a port that appears inside that window is not visible yet.
    // Deliberate here — what these tests ask is what the runtime does about
    // something it can see, not how quickly it notices.
    tokio::time::sleep(std::time::Duration::from_millis(1_700)).await;

    let mut service = declare(runtime, &workspace.id, dir.path(), "web", stays_up(), &[], false);
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

    if !adoption_is_possible(&runtime, &service) {
        let _ = child.kill();
        return;
    }

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

    if !adoption_is_possible(&runtime, &service) {
        let _ = child.kill();
        return;
    }

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

    declare(&runtime, &workspace.id, dir.path(), "api", stays_up(), &["db"], false);

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

    declare(&runtime, &workspace.id, dir.path(), "a", stays_up(), &["b"], false);
    declare(&runtime, &workspace.id, dir.path(), "b", stays_up(), &["a"], false);

    let findings = runtime.diagnose().unwrap();
    assert!(
        findings.iter().any(|f| f.message.contains("depend on each other")),
        "{findings:?}"
    );
}

#[tokio::test]
async fn a_stack_step_that_was_removed_is_found() {
    let dir = repo();
    let runtime = Runtime::in_memory().unwrap();
    let project = runtime.add_project(dir.path(), None).unwrap();
    let workspace = runtime
        .store()
        .list_workspaces(&project.project.id)
        .unwrap()
        .remove(0);

    let web = declare(&runtime, &workspace.id, dir.path(), "web", stays_up(), &[], false);
    runtime
        .set_stack(&workspace.id, "dev", vec!["web".to_string()])
        .unwrap();
    // Steps are checked when a stack is declared; a service can go afterwards.
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

    let db = declare(&runtime, &workspace.id, dir.path(), "db", stays_up(), &[], false);
    declare(&runtime, &workspace.id, dir.path(), "api", stays_up(), &["db"], false);
    let _ = db;

    assert!(runtime.diagnose().unwrap().is_empty(), "quiet is the point");
}

#[tokio::test]
async fn a_failure_reports_only_the_run_that_failed() {
    // Output is kept across restarts on purpose, so a service failing a second
    // time has both failures in its log. Reading the tail of all of it produces
    // an error message assembled from two different attempts, which is worse
    // than no message: it looks like one.
    let dir = repo();
    let logs = tempfile::tempdir().unwrap();
    let runtime = Runtime::in_memory_with_logs(logs.path()).unwrap();
    let project = runtime.add_project(dir.path(), None).unwrap();
    let workspace = runtime
        .store()
        .list_workspaces(&project.project.id)
        .unwrap()
        .remove(0);

    let say = dir.path().join("say.py");
    std::fs::write(&say, "import sys\nprint(sys.argv[1], file=sys.stderr)\nsys.exit(1)\n").unwrap();

    let mut service = declare(
        &runtime,
        &workspace.id,
        dir.path(),
        "flaky",
        &format!("{} {} FIRST_FAILURE", python(), say.display()),
        &[],
        true,
    );

    let _ = runtime.start_service(&service.id, Default::default()).await;

    // Long enough that the second run's lines are stamped after the second
    // instance began, which is the boundary being relied on.
    tokio::time::sleep(std::time::Duration::from_millis(1_200)).await;

    service.command = format!("{} {} SECOND_FAILURE", python(), say.display());
    runtime.store().upsert_service(&service).unwrap();
    let _ = runtime.start_service(&service.id, Default::default()).await;

    let failures = runtime.failures(20).unwrap();
    let flaky = failures
        .iter()
        .find(|failure| failure.subject.ends_with("/flaky"))
        .expect("a failing service should be reported");

    let said = flaky.detail.join("\n");
    assert!(said.contains("SECOND_FAILURE"), "{said}");
    assert!(
        !said.contains("FIRST_FAILURE"),
        "the previous run's output leaked into this one: {said}"
    );
}

/// Declaring or dropping a group has to reach a window that shows groups.
///
/// The checkout view carries its groups, so a group made from the terminal is
/// invisible in an open window until something unrelated happens to it. The
/// registry edits that concern services already announce themselves; this is
/// the same obligation, and it was missed when groups joined the view.
#[tokio::test]
async fn declaring_and_dropping_a_group_is_announced() {
    let dir = repo();
    let runtime = Runtime::in_memory().unwrap();
    let project = runtime.add_project(dir.path(), None).unwrap();
    let workspace = runtime
        .store()
        .list_workspaces(&project.project.id)
        .unwrap()
        .remove(0);
    declare(&runtime, &workspace.id, dir.path(), "web", stays_up(), &[], false);

    let mut events = runtime.events().subscribe();
    runtime
        .set_stack(&workspace.id, "dev", vec!["web".to_string()])
        .unwrap();
    assert!(
        matches!(events.try_recv(), Ok(runtime_core::events::RuntimeEvent::WorkspaceChanged { .. })),
        "declaring a group said nothing"
    );

    assert!(runtime.remove_stack(&workspace.id, "dev").unwrap());
    assert!(
        matches!(events.try_recv(), Ok(runtime_core::events::RuntimeEvent::WorkspaceChanged { .. })),
        "dropping a group said nothing"
    );
}

/// A service in no stack cannot be brought up by asking for it by name.
///
/// The rule the panel and the window both show, held where it is actually
/// enforced. It lived in a button first, which left the same question answered
/// three ways — the panel refused, the window offered, the CLI obliged.
///
/// The two things it must not break are the reasons a loose service still gets
/// started: it is depended on by something that is being started, or it is a
/// member of a stack being run. Both go through the runtime rather than
/// through a request naming it, which is exactly the distinction being drawn.
#[tokio::test]
async fn a_service_in_no_stack_is_refused_by_name_but_still_started_as_a_dependency() {
    let dir = repo();
    let runtime = Runtime::in_memory().unwrap();
    let project = runtime.add_project(dir.path(), None).unwrap();
    let workspace = runtime
        .store()
        .list_workspaces(&project.project.id)
        .unwrap()
        .remove(0);

    let base = declare(&runtime, &workspace.id, dir.path(), "base", stays_up(), &[], false);
    let front = declare(&runtime, &workspace.id, dir.path(), "front", stays_up(), &["base"], false);

    // Neither is in a stack yet: both refused.
    assert!(runtime.require_in_a_stack(&base.id).is_err());
    assert!(runtime.require_in_a_stack(&front.id).is_err());

    runtime
        .set_stack(&workspace.id, "dev", vec!["front".to_string()])
        .unwrap();

    // `front` is named by the stack, so asking for it is allowed.
    runtime.require_in_a_stack(&front.id).unwrap();
    // `base` still is not — but running the stack brings it up anyway.
    assert!(runtime.require_in_a_stack(&base.id).is_err());

    let done = runtime.run_stack(&workspace.id, "dev").await.unwrap();
    assert_eq!(done, vec!["front".to_string()], "the stack reports its own members");

    let up = runtime.service_view(&base).unwrap();
    assert!(up.status.is_live(), "a dependency outside the stack was left down: {up:?}");

    runtime.stop_stack(&workspace.id, "dev").await.unwrap();
    let _ = runtime
        .stop_service(&base.id, std::time::Duration::from_secs(5))
        .await;
}

/// A stack is declared once for a project and applies to every checkout of it.
///
/// It is stored on the project's root, and `run_stack` always read it there
/// while everything else read it from whichever checkout was asked. So a
/// worktree could be shown a stack by `stack list` and told by the start rule
/// that its services were in none — the same question, two answers.
#[tokio::test]
async fn a_stack_declared_once_reaches_every_checkout() {
    let dir = repo();
    let runtime = Runtime::in_memory().unwrap();
    let project = runtime.add_project(dir.path(), None).unwrap();
    let root = runtime
        .store()
        .list_workspaces(&project.project.id)
        .unwrap()
        .remove(0);
    let web = declare(&runtime, &root.id, dir.path(), "web", stays_up(), &[], false);
    runtime
        .set_stack(&root.id, "dev", vec!["web".to_string()])
        .unwrap();

    // A second checkout of the same project, with its own copy of the service.
    let branch = tempfile::tempdir().unwrap();
    let other = runtime.register_worktree(&project.project.id, branch.path()).unwrap();
    let elsewhere = declare(&runtime, &other.id, branch.path(), "web", stays_up(), &[], false);

    assert_eq!(
        runtime.stacks_for(&other.id).unwrap().len(),
        1,
        "the checkout cannot see the project's stack"
    );
    runtime
        .require_in_a_stack(&elsewhere.id)
        .expect("shown a stack but refused a start");
    let _ = web;
}

/// A stack of one-shots is not "up" before it has ever been run.
///
/// The count of what is running used to include every one-shot, on the
/// reasoning that a migration which has finished leaves nothing behind. But
/// the test was "is a one-shot", not "has run", so a stack whose only member
/// was a migration reported itself fully up the moment it was declared — and
/// the window drew a live dot and a Stop button for a process that had never
/// existed, which is what a stop cannot do anything about.
#[tokio::test]
async fn a_one_shot_that_has_never_run_is_not_counted_as_up() {
    let dir = repo();
    let runtime = Runtime::in_memory().unwrap();
    let project = runtime.add_project(dir.path(), None).unwrap();
    let workspace = runtime
        .store()
        .list_workspaces(&project.project.id)
        .unwrap()
        .remove(0);

    declare(&runtime, &workspace.id, dir.path(), "migrate", &creates(Path::new("done")), &[], true);
    runtime
        .set_stack(&workspace.id, "setup", vec!["migrate".to_string()])
        .unwrap();

    let view = runtime
        .stack_views(&workspace.id)
        .unwrap()
        .into_iter()
        .find(|stack| stack.stack.name == "setup")
        .expect("no such stack");

    assert_eq!(view.running, 0, "a migration that never ran was counted as up");
    // And it is still identifiable as the kind of thing that is run rather
    // than started, which is how a surface words it.
    assert!(view.flow.iter().all(|node| node.one_shot), "{:?}", view.flow);
}

/// Adopting exists so a running thing can be started again later.
///
/// A service in no stack cannot be started by name, so declaring one and
/// leaving it outside every stack undid this command's purpose one step after
/// it succeeded — the error even told you to put it in a stack, which is what
/// adopting was supposed to have arranged. Running `adopt` is somebody saying
/// they want this managed, which is the declaration the rule asks for.
#[tokio::test]
async fn an_adopted_service_can_be_started() {
    let dir = repo();
    let runtime = Runtime::in_memory().unwrap();
    let (service, mut child) = adopted(&dir, &runtime).await;

    if !adoption_is_possible(&runtime, &service) {
        let _ = child.kill();
        return;
    }
    let port = service.preferred_port.expect("the fixture sets one");

    let outcome = runtime.adopt_port(port, false, None).unwrap();
    let stack = outcome
        .stack
        .expect("adopting left the service in no stack, so it cannot be started");
    let adopted_id = outcome.service.service.id.clone();

    runtime
        .require_in_a_stack(&adopted_id)
        .expect("adopting left the service unstartable, which is what it exists to prevent");
    assert!(
        runtime
            .stacks_for(&outcome.service.service.workspace_id)
            .unwrap()
            .iter()
            .any(|candidate| candidate.name == stack),
        "the stack it reported does not exist"
    );

    let _ = child.kill();
}

/// A service is started as part of its stack, never on its own.
///
/// The unit somebody declared is the unit that runs. Bringing one member up by
/// hand leaves the rest down while every list reads as though the stack is
/// partly up, and taking one down out from under the others is the same thing
/// backwards.
///
/// What this must not break is the reason a member comes up at all: the stack
/// starting it, and anything it depends on being brought up first. Those go
/// through the runtime rather than through a request naming a service, which
/// is the distinction the rule is drawn on.
#[tokio::test]
async fn a_member_is_refused_alone_and_started_by_its_stack() {
    let dir = repo();
    let runtime = Runtime::in_memory().unwrap();
    let project = runtime.add_project(dir.path(), None).unwrap();
    let workspace = runtime
        .store()
        .list_workspaces(&project.project.id)
        .unwrap()
        .remove(0);

    let base = declare(&runtime, &workspace.id, dir.path(), "base", stays_up(), &[], false);
    let front = declare(&runtime, &workspace.id, dir.path(), "front", stays_up(), &["base"], false);
    runtime
        .set_stack(&workspace.id, "dev", vec!["front".to_string()])
        .unwrap();

    for verb in ["started", "stopped", "restarted"] {
        let refused = runtime.refuse_alone(&front.id, verb).unwrap_err().to_string();
        assert!(refused.contains("dev"), "{verb}: {refused}");
        assert!(refused.contains("not one at a time"), "{verb}: {refused}");
    }

    // And the stack brings it up, along with what it depends on.
    let done = runtime.run_stack(&workspace.id, "dev").await.unwrap();
    assert_eq!(done, vec!["front".to_string()]);
    assert!(
        runtime.service_view(&base).unwrap().status.is_live(),
        "a dependency was left down"
    );

    runtime.stop_stack(&workspace.id, "dev").await.unwrap();
    let _ = runtime
        .stop_service(&base.id, std::time::Duration::from_secs(5))
        .await;
}

/// A port somebody wrote down is a port they meant.
///
/// Moving the service to the next free one keeps the start succeeding and
/// turns the number they wrote into a suggestion — and it is usually written
/// down because something else has it fixed: a proxy, a callback URL, a
/// colleague's notes. So a declared port that is taken stops the run and names
/// the holder, and a member with no port declared still takes the next free
/// one, because nobody said which it should be.
#[tokio::test]
async fn a_declared_port_that_is_taken_stops_the_run() {
    let dir = repo();
    let runtime = Runtime::in_memory().unwrap();
    let project = runtime.add_project(dir.path(), None).unwrap();
    let workspace = runtime
        .store()
        .list_workspaces(&project.project.id)
        .unwrap()
        .remove(0);

    // A port held by something outside the runtime, for the length of the test.
    let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = held.local_addr().unwrap().port();

    let mut service = declare(&runtime, &workspace.id, dir.path(), "web", stays_up(), &[], false);
    service.preferred_port = Some(port);
    runtime.store().upsert_service(&service).unwrap();
    runtime
        .set_stack(&workspace.id, "dev", vec!["web".to_string()])
        .unwrap();

    let refused = runtime.run_stack(&workspace.id, "dev").await.unwrap_err();
    match refused {
        runtime_types::RuntimeError::PortConflict { port: reported, holder } => {
            assert_eq!(reported, port);
            assert!(!holder.is_empty(), "the holder was not named");
        }
        other => panic!("expected a port conflict, got {other:?}"),
    }

    // Nothing was started, so nothing is left running behind the refusal.
    assert!(
        !runtime.service_view(&service).unwrap().status.is_live(),
        "the member started anyway"
    );
    drop(held);
}

/// Port zero means "any free one", chosen again at every start.
///
/// Three states, and a socket already has names for all of them: no port at
/// all for something that does not listen, a fixed number for something that
/// must have it, and zero for whatever is free. Nothing is written back — the
/// point of asking for any is that the next run can have another, which is
/// what keeps it working when this one gets taken.
///
/// Tested with a service that reads `$PORT`, because that is the only kind for
/// which asking for any port means anything: the runtime chooses and tells,
/// and a service that ignores the telling would be listening somewhere else.
#[tokio::test]
async fn any_port_is_chosen_at_each_start_and_not_written_down() {
    let dir = repo();
    let runtime = Runtime::in_memory().unwrap();
    let project = runtime.add_project(dir.path(), None).unwrap();
    let workspace = runtime
        .store()
        .list_workspaces(&project.project.id)
        .unwrap()
        .remove(0);

    // How a shell spells a variable, which is not the same shell on both
    // platforms: `cmd.exe` reads `%PORT%` and would treat `$PORT` as a literal
    // argument, so the server would bind nothing and the test would blame the
    // allocator.
    let port_var = if cfg!(windows) { "%PORT%" } else { "$PORT" };
    let serves = format!("{} -m http.server {port_var} --bind 127.0.0.1", python());
    let mut service = declare(&runtime, &workspace.id, dir.path(), "web", &serves, &[], false);
    service.preferred_port = Some(runtime_types::ANY_PORT);
    runtime.store().upsert_service(&service).unwrap();
    runtime
        .set_stack(&workspace.id, "dev", vec!["web".to_string()])
        .unwrap();

    runtime.run_stack(&workspace.id, "dev").await.unwrap();
    let chosen = runtime
        .service_view(&service)
        .unwrap()
        .actual_port
        .expect("no port was chosen for a service that asked for any");
    assert!(chosen > 0, "zero is the request, not an answer");

    // The request is still "any": nothing was written back, so the next start
    // is free to land somewhere else.
    let stored = runtime.require_service(&service.id).unwrap();
    assert_eq!(
        stored.preferred_port,
        Some(runtime_types::ANY_PORT),
        "the chosen port was written down as if it had been asked for"
    );

    runtime.stop_stack(&workspace.id, "dev").await.unwrap();
}
