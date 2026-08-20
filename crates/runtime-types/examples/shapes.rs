//! The exact JSON the daemon sends, for the frontend to be tested against.
//!
//! Written by the types themselves rather than by hand. The frontend's idea of
//! a payload is a TypeScript interface somebody wrote from memory, and the one
//! that took the window down was wrong in the only way that matters: it said a
//! field was always there, and `skip_serializing_if` means it is absent when
//! empty. A fixture copied from the wire cannot make that mistake.

use std::collections::BTreeMap;

use chrono::Utc;
use runtime_types::*;

fn main() {
    let mut out = serde_json::Map::new();

    // Every optional field present.
    out.insert(
        "supervised_full".to_string(),
        serde_json::to_value(SupervisedView {
            name: "flip7".to_string(),
            supervisor: "pm2".to_string(),
            status: "online".to_string(),
            pid: Some(29106),
            command: "node server.mjs".to_string(),
            restarts: 22,
            ports: vec![3007],
            url: Some("http://localhost:3007".to_string()),
            restart_warning: Some("holds a development build".to_string()),
        })
        .unwrap(),
    );

    // Every optional field absent, which is what a stopped entry looks like.
    out.insert(
        "supervised_minimal".to_string(),
        serde_json::to_value(SupervisedView {
            name: "loom-tunnel".to_string(),
            supervisor: "pm2".to_string(),
            status: "stopped".to_string(),
            pid: None,
            command: "cloudflared tunnel run".to_string(),
            restarts: 0,
            ports: Vec::new(),
            url: None,
            restart_warning: None,
        })
        .unwrap(),
    );

    let service = Service {
        id: ServiceId::from("svc"),
        workspace_id: WorkspaceId::from("ws"),
        name: "web".to_string(),
        service_type: ServiceType::Web,
        command: "pnpm dev".to_string(),
        cwd: "/repo".into(),
        env: BTreeMap::new(),
        preferred_port: Some(3000),
        health_check: None,
        auto_start: false,
        conflict_policy: ConflictPolicy::Reuse,
        depends_on: Vec::new(),
        one_shot: false,
    };

    out.insert(
        "service_minimal".to_string(),
        serde_json::to_value(ServiceView {
            service: service.clone(),
            status: ServiceStatus::Stopped,
            instance: None,
            actual_port: None,
            url: None,
            managed: false,
            supervisor: None,
            supervisor_entry: None,
        })
        .unwrap(),
    );

    out.insert(
        "service_full".to_string(),
        serde_json::to_value(ServiceView {
            service: Service {
                depends_on: vec!["db".to_string()],
                one_shot: false,
                ..service.clone()
            },
            status: ServiceStatus::Healthy,
            instance: Some(RuntimeInstance {
                id: InstanceId::from("inst"),
                service_id: ServiceId::from("svc"),
                pid: 42,
                process_start_time: 0,
                status: ServiceStatus::Healthy,
                port: Some(3000),
                started_at: Utc::now(),
                stopped_at: None,
                exit_code: None,
                started_by: StartedBy::ClaudeCode,
                owner_session: None,
            }),
            actual_port: Some(3000),
            url: Some("http://localhost:3000".to_string()),
            managed: false,
            supervisor: Some("pm2".to_string()),
            supervisor_entry: Some("flip7".to_string()),
        })
        .unwrap(),
    );

    // A one-shot that has run: the row reads its exit code.
    out.insert(
        "service_one_shot_ran".to_string(),
        serde_json::to_value(ServiceView {
            service: Service {
                one_shot: true,
                preferred_port: None,
                ..service.clone()
            },
            status: ServiceStatus::Stopped,
            instance: Some(RuntimeInstance {
                id: InstanceId::from("inst"),
                service_id: ServiceId::from("svc"),
                pid: 0,
                process_start_time: 0,
                status: ServiceStatus::Stopped,
                port: None,
                started_at: Utc::now(),
                stopped_at: Some(Utc::now()),
                exit_code: Some(0),
                started_by: StartedBy::Unknown,
                owner_session: None,
            }),
            actual_port: None,
            url: None,
            managed: false,
            supervisor: None,
            supervisor_entry: None,
        })
        .unwrap(),
    );

    out.insert(
        "external_minimal".to_string(),
        serde_json::to_value(ExternalService {
            port: 5555,
            pid: 1,
            container: None,
            cwd: None,
            command_line: None,
            url: None,
            supervisor: None,
        })
        .unwrap(),
    );

    out.insert(
        "container_minimal".to_string(),
        serde_json::to_value(ContainerView {
            name: "db".to_string(),
            service: None,
            image: "postgres".to_string(),
            status: "exited".to_string(),
            health: None,
            ports: Vec::new(),
            url: None,
        })
        .unwrap(),
    );

    out.insert(
        "finding".to_string(),
        serde_json::to_value(Finding {
            subject: "Loom/api".to_string(),
            message: "depends on 'db', which this checkout does not declare".to_string(),
            certain: true,
        })
        .unwrap(),
    );

    println!("{}", serde_json::to_string_pretty(&out).unwrap());
}
