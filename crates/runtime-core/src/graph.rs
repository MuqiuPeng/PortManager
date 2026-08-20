//! Working out what has to start before what.
//!
//! Ordering is the easy half. The hard half is that a dependency is usually
//! already running — under PM2, in a terminal, from an earlier session — and
//! the useful behaviour there is to leave it alone. Restarting it would take a
//! working service down to reach a state it was already in, and on a machine
//! where something else supervises it, would lose a race for the port as well.
//!
//! So the plan this produces says, for each step, whether it needs starting or
//! is already satisfied; a caller that finds everything satisfied does nothing
//! at all.

use std::collections::{BTreeMap, BTreeSet};

use runtime_types::{Result, RuntimeError, Service, ServiceId};

/// One step of a start plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub service_id: ServiceId,
    pub name: String,
    /// False when it is already up and nothing needs doing.
    pub needs_start: bool,
    /// True for a step that runs to completion rather than staying up.
    pub one_shot: bool,
}

/// Order the services that must be up before `roots`, `roots` last.
///
/// `is_live` decides whether a service already counts as satisfied. It is
/// asked about every service in the graph, including ones the runtime did not
/// start: a dependency held by PM2 is a dependency met.
pub fn plan(
    roots: &[Service],
    all: &[Service],
    is_live: impl Fn(&Service) -> bool,
) -> Result<Vec<Step>> {
    let by_name: BTreeMap<&str, &Service> =
        all.iter().map(|service| (service.name.as_str(), service)).collect();

    let mut ordered: Vec<&Service> = Vec::new();
    let mut done: BTreeSet<&str> = BTreeSet::new();
    let mut visiting: Vec<&str> = Vec::new();

    for root in roots {
        visit(root, &by_name, &mut ordered, &mut done, &mut visiting)?;
    }

    Ok(ordered
        .into_iter()
        .map(|service| Step {
            service_id: service.id.clone(),
            name: service.name.clone(),
            // A one-shot is never "already satisfied": a migration that ran an
            // hour ago says nothing about whether it needs running now, and
            // the cheap thing to do with one that is already applied is to run
            // it again.
            needs_start: service.one_shot || !is_live(service),
            one_shot: service.one_shot,
        })
        .collect())
}

fn visit<'a>(
    service: &'a Service,
    by_name: &BTreeMap<&'a str, &'a Service>,
    ordered: &mut Vec<&'a Service>,
    done: &mut BTreeSet<&'a str>,
    visiting: &mut Vec<&'a str>,
) -> Result<()> {
    if done.contains(service.name.as_str()) {
        return Ok(());
    }
    if visiting.contains(&service.name.as_str()) {
        // Naming the loop, because "circular dependency" without the cycle
        // leaves the reader to find it themselves.
        let mut cycle: Vec<&str> = visiting
            .iter()
            .skip_while(|name| **name != service.name.as_str())
            .copied()
            .collect();
        cycle.push(service.name.as_str());
        return Err(RuntimeError::invalid(format!(
            "these services depend on each other: {}",
            cycle.join(" -> ")
        )));
    }

    visiting.push(service.name.as_str());
    for dependency in &service.depends_on {
        let Some(next) = by_name.get(dependency.as_str()) else {
            return Err(RuntimeError::invalid(format!(
                "'{}' depends on '{dependency}', which this checkout does not declare",
                service.name
            )));
        };
        visit(next, by_name, ordered, done, visiting)?;
    }
    visiting.pop();

    done.insert(service.name.as_str());
    ordered.push(service);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use runtime_types::{ConflictPolicy, ServiceType, WorkspaceId};

    fn service(name: &str, depends_on: &[&str]) -> Service {
        Service {
            id: ServiceId::from(name),
            workspace_id: WorkspaceId::from("ws"),
            name: name.to_string(),
            service_type: ServiceType::Web,
            command: format!("run {name}"),
            cwd: "/repo".into(),
            env: Default::default(),
            preferred_port: None,
            health_check: None,
            auto_start: false,
            conflict_policy: ConflictPolicy::Fail,
            depends_on: depends_on.iter().map(|d| d.to_string()).collect(),
            one_shot: false,
        }
    }

    fn names(steps: &[Step]) -> Vec<&str> {
        steps.iter().map(|step| step.name.as_str()).collect()
    }

    #[test]
    fn dependencies_come_before_what_needs_them() {
        let db = service("db", &[]);
        let api = service("api", &["db"]);
        let web = service("web", &["api"]);
        let all = vec![db, api, web.clone()];

        let steps = plan(&[web], &all, |_| false).unwrap();
        assert_eq!(names(&steps), ["db", "api", "web"]);
    }

    #[test]
    fn a_shared_dependency_is_visited_once() {
        let db = service("db", &[]);
        let api = service("api", &["db"]);
        let worker = service("worker", &["db"]);
        let all = vec![db, api.clone(), worker.clone()];

        let steps = plan(&[api, worker], &all, |_| false).unwrap();
        assert_eq!(names(&steps), ["db", "api", "worker"]);
    }

    #[test]
    fn something_already_running_is_left_alone() {
        // The everyday case on a machine where PM2 or a terminal got there
        // first: the dependency is met, and restarting it would take a working
        // service down to reach the state it was already in.
        let db = service("db", &[]);
        let api = service("api", &["db"]);
        let all = vec![db, api.clone()];

        let steps = plan(&[api], &all, |service| service.name == "db").unwrap();
        assert_eq!(names(&steps), ["db", "api"]);
        assert!(!steps[0].needs_start);
        assert!(steps[1].needs_start);
    }

    #[test]
    fn a_one_shot_runs_even_when_it_ran_before() {
        let mut migrate = service("migrate", &[]);
        migrate.one_shot = true;
        let api = service("api", &["migrate"]);
        let all = vec![migrate, api.clone()];

        // `is_live` says yes to everything, and the migration still runs.
        let steps = plan(&[api], &all, |_| true).unwrap();
        assert!(steps[0].needs_start);
        assert!(steps[0].one_shot);
        assert!(!steps[1].needs_start);
    }

    #[test]
    fn a_cycle_is_reported_with_the_cycle_in_it() {
        let a = service("a", &["b"]);
        let b = service("b", &["a"]);
        let all = vec![a.clone(), b];

        let error = plan(&[a], &all, |_| false).unwrap_err().to_string();
        assert!(error.contains("a -> b -> a"), "{error}");
    }

    #[test]
    fn a_service_depending_on_itself_is_a_cycle() {
        let a = service("a", &["a"]);
        let all = vec![a.clone()];
        assert!(plan(&[a], &all, |_| false).is_err());
    }

    #[test]
    fn a_missing_dependency_names_what_is_missing() {
        let api = service("api", &["db"]);
        let all = vec![api.clone()];

        let error = plan(&[api], &all, |_| false).unwrap_err().to_string();
        assert!(error.contains("'api' depends on 'db'"), "{error}");
    }

    #[test]
    fn a_service_with_no_dependencies_is_one_step() {
        let web = service("web", &[]);
        let all = std::slice::from_ref(&web);
        let steps = plan(all, all, |_| false).unwrap();
        assert_eq!(names(&steps), ["web"]);
    }
}
