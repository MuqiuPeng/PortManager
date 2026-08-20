//! Every frame must survive the wire.
//!
//! Serde's internally tagged enums cannot encode a newtype variant wrapping a
//! sequence or a string — and the failure is at *runtime*, while writing the
//! response, so the daemon drops the connection having said nothing. That has
//! happened twice: once to `ResponseBody`'s collections, once to
//! `RuntimeError`'s messages. This test exists so it cannot happen a third time
//! without a red test.

use std::collections::BTreeMap;

use chrono::Utc;
use runtime_core::discover::Discovery;
use runtime_ipc::protocol::{Frame, Request, ResponseBody};
use runtime_types::*;

fn service() -> Service {
    Service {
        id: ServiceId::from("svc"),
        workspace_id: WorkspaceId::from("ws"),
        name: "web".to_string(),
        service_type: ServiceType::Web,
        command: "pnpm dev".to_string(),
        cwd: "/repo".into(),
        env: BTreeMap::from([("A".to_string(), "1".to_string())]),
        preferred_port: Some(3000),
        health_check: Some(HealthCheck::Tcp { port: None }),
        auto_start: false,
        conflict_policy: ConflictPolicy::AllocateNext,
        depends_on: vec!["db".to_string()],
        one_shot: false,
    }
}

fn workspace() -> Workspace {
    Workspace {
        id: WorkspaceId::from("ws"),
        project_id: ProjectId::from("proj"),
        path: "/repo".into(),
        git_branch: Some("main".to_string()),
        git_commit: None,
        worktree: false,
        port_offset: 0,
        created_at: Utc::now(),
    }
}

fn service_view() -> ServiceView {
    ServiceView {
        service: service(),
        status: ServiceStatus::Healthy,
        instance: None,
        actual_port: Some(3000),
        url: Some("http://localhost:3000".to_string()),
        managed: true,
        supervisor: Some("pm2".to_string()),
        supervisor_entry: Some("flip7".to_string()),
    }
}

fn project_view() -> ProjectView {
    ProjectView {
        project: Project {
            id: ProjectId::from("proj"),
            name: "shop".to_string(),
            root_path: "/repo".into(),
            repository_url: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        },
        workspaces: vec![WorkspaceView {
            workspace: workspace(),
            services: vec![service_view()],
            external: vec![ExternalService {
                port: 5555,
                pid: 1,
                container: None,
                cwd: None,
                command_line: Some("node".to_string()),
                url: None,
                supervisor: Some("pm2".to_string()),
            }],
            supervised: vec![],
            containers: vec![ContainerView {
                name: "db".to_string(),
                service: Some("db".to_string()),
                image: "postgres:16".to_string(),
                status: "running".to_string(),
                health: Some("healthy".to_string()),
                ports: vec![5432],
                url: None,
            }],
        }],
        running_services: 1,
        total_services: 1,
        external_services: 1,
    }
}

fn port_owner() -> PortOwner {
    PortOwner {
        port: 3000,
        pid: 42,
        executable: Some("/usr/local/bin/node".to_string()),
        cwd: Some("/repo".into()),
        command_line: Some("node server.js".to_string()),
        project_id: Some(ProjectId::from("proj")),
        project_name: Some("shop".to_string()),
        workspace_id: Some(WorkspaceId::from("ws")),
        git_branch: Some("main".to_string()),
        service_id: Some(ServiceId::from("svc")),
        service_name: Some("web".to_string()),
        started_by: Some(StartedBy::Cli),
        container: None,
        supervisor: Some("pm2".to_string()),
        managed: true,
    }
}

/// One of every `ResponseBody` variant.
fn responses() -> Vec<ResponseBody> {
    vec![
        ResponseBody::Pong { protocol_version: 1 },
        ResponseBody::Info(DaemonInfo {
            version: "0.1.0".to_string(),
            pid: 1,
            socket_path: "/tmp/s.sock".into(),
            database_path: "/tmp/db".into(),
            platform: "macos".to_string(),
            uptime_seconds: 3,
        }),
        ResponseBody::Projects { items: vec![project_view()] },
        ResponseBody::Project(project_view()),
        ResponseBody::Discoveries {
            items: vec![Discovery {
                root_path: "/repo".into(),
                name: "shop".to_string(),
                running: true,
                ports: vec![3000],
                markers: vec!["git".to_string()],
                git_branch: Some("main".to_string()),
                suggested_services: vec!["web".to_string()],
                registered: false,
            }],
        },
        ResponseBody::Workspaces { items: vec![workspace()] },
        ResponseBody::Workspace(workspace()),
        ResponseBody::Services { items: vec![service_view()] },
        ResponseBody::Service(service_view()),
        ResponseBody::Config(ProjectConfig {
            name: Some("shop".to_string()),
            services: BTreeMap::from([(
                "web".to_string(),
                ServiceConfig {
                    command: "pnpm dev".to_string(),
                    port: Some(3000),
                    cwd: None,
                    service_type: Some(ServiceType::Web),
                    env: BTreeMap::new(),
                    health: None,
                    auto_start: false,
                    on_conflict: None,
                    depends_on: vec!["db".to_string()],
                    one_shot: true,
                },
            )]),
        }),
        ResponseBody::Container(ContainerView {
            name: "db".to_string(),
            service: None,
            image: "postgres:16".to_string(),
            status: "exited".to_string(),
            health: None,
            ports: vec![],
            url: None,
        }),
        ResponseBody::Started(StartOutcome {
            warning: Some("rewrites a build 'flip7' serves from".to_string()),
            service: service_view(),
            reused: false,
            reservation: Some(PortReservation {
                port: 3000,
                preferred_port: Some(3000),
                reallocated: false,
                policy: ConflictPolicy::Fail,
                conflict: Some(port_owner()),
            }),
        }),
        ResponseBody::Health(HealthReport {
            service_id: ServiceId::from("svc"),
            status: ServiceStatus::Unhealthy,
            detail: Some("refused".to_string()),
            checked_port: Some(3000),
        }),
        ResponseBody::Port(PortStatus {
            port: 3000,
            available: false,
            owner: Some(port_owner()),
            lease_status: Some(PortLeaseStatus::Active),
            suggested_port: Some(3001),
        }),
        ResponseBody::Ports { items: vec![port_owner()] },
        ResponseBody::Reservation(PortReservation {
            port: 3001,
            preferred_port: Some(3000),
            reallocated: true,
            policy: ConflictPolicy::AllocateNext,
            conflict: None,
        }),
        ResponseBody::Logs {
            items: vec![LogLine {
                seq: 0,
                service_id: ServiceId::from("svc"),
                stream: LogStream::Stderr,
                timestamp: Utc::now(),
                message: "boom".to_string(),
            }],
        },
        ResponseBody::Setting { value: Some("{}".to_string()) },
        ResponseBody::Launches {
            items: vec![LaunchObservation {
                id: "1-abc".to_string(),
                command: "cd frontend && PORT=4000 pnpm dev".to_string(),
                cwd: "/repo".into(),
                source: StartedBy::ClaudeCode,
                session: Some("s1".to_string()),
                observed_at: Utc::now(),
                state: LaunchState::Bound,
                port: Some(4000),
                pid: Some(42),
                service_id: Some(ServiceId::from("svc")),
            }],
        },
        ResponseBody::Adopted(AdoptOutcome {
            service: service_view(),
            command_source: CommandSource::ProcessArgv,
            declared: false,
            replaced_command: Some("npm run dev".to_string()),
            supervisor: Some("pm2".to_string()),
        }),
        ResponseBody::Supervised(SupervisedView {
            name: "flip7".to_string(),
            supervisor: "pm2".to_string(),
            status: "online".to_string(),
            pid: Some(29106),
            command: "server.mjs".to_string(),
            restarts: 22,
            ports: vec![3007],
            url: Some("http://localhost:3007".to_string()),
            restart_warning: Some("holds a development build".to_string()),
        }),
        ResponseBody::Tasks {
            items: vec![Task {
                id: TaskId::from("task"),
                workspace_id: WorkspaceId::from("ws"),
                name: "dev".to_string(),
                steps: vec!["migrate".to_string(), "api".to_string()],
            }],
        },
        ResponseBody::TaskRun {
            steps: vec!["migrate (ran)".to_string(), "api".to_string()],
        },
        ResponseBody::Sessions {
            items: vec![AgentSession {
                id: SessionId::from("sess"),
                provider: "anthropic".to_string(),
                client: "claude-code".to_string(),
                cwd: None,
                project_id: None,
                started_at: Utc::now(),
                last_seen_at: Utc::now(),
            }],
        },
        ResponseBody::Session(AgentSession {
            id: SessionId::from("sess"),
            provider: "openai".to_string(),
            client: "codex".to_string(),
            cwd: Some("/repo".into()),
            project_id: Some(ProjectId::from("proj")),
            started_at: Utc::now(),
            last_seen_at: Utc::now(),
        }),
        ResponseBody::Done { ok: true },
    ]
}

#[test]
fn every_response_survives_a_round_trip() {
    for body in responses() {
        let label = format!("{body:?}");
        let frame = Frame::Response { id: 1, result: body };

        let encoded = serde_json::to_string(&frame)
            .unwrap_or_else(|err| panic!("cannot encode {label}: {err}"));
        let decoded: Frame = serde_json::from_str(&encoded)
            .unwrap_or_else(|err| panic!("cannot decode {label}: {err}\n{encoded}"));
        assert_eq!(decoded, frame, "changed shape: {label}");
    }
}

#[test]
fn every_request_survives_a_round_trip() {
    let requests = vec![
        Request::Ping,
        Request::DaemonInfo,
        Request::Shutdown,
        Request::ListProjects,
        Request::DiscoverProjects { paths: vec!["/a".into()], adopt: true },
        Request::GetProject { selector: "shop".to_string() },
        Request::AddProject { path: "/repo".into(), name: Some("shop".to_string()) },
        Request::RemoveProject { selector: "shop".to_string() },
        Request::ListWorktrees { selector: "shop".to_string() },
        Request::RegisterWorktree { selector: "shop".to_string(), path: "/wt".into() },
        Request::ListServices { project: None },
        Request::GetService { project: None, service: "web".to_string() },
        Request::UpdateService {
            project: None,
            service: "web".to_string(),
            patch: ServicePatch {
                preferred_port: Some(None),
                env: BTreeMap::from([("A".to_string(), "1".to_string())]),
                remove_env: vec!["B".to_string()],
                ..Default::default()
            },
        },
        Request::AddService {
            selector: "shop".to_string(),
            name: "worker".to_string(),
            config: ServiceConfig {
                command: "pnpm worker".to_string(),
                port: None,
                cwd: None,
                service_type: None,
                env: BTreeMap::new(),
                health: None,
                auto_start: false,
                on_conflict: None,
                depends_on: vec!["db".to_string()],
                one_shot: true,
            },
        },
        Request::RemoveService { project: None, service: "web".to_string() },
        Request::ExportConfig { selector: "shop".to_string() },
        Request::RecordLaunch {
            command: "cd frontend && PORT=4000 pnpm dev".to_string(),
            cwd: "/repo".into(),
            source: Some("claude-code".to_string()),
            session: Some("s1".to_string()),
        },
        Request::ListLaunches,
        Request::ListTasks { selector: ".".to_string() },
        Request::SetTask {
            selector: ".".to_string(),
            name: "dev".to_string(),
            steps: vec!["migrate".to_string(), "api".to_string()],
        },
        Request::RemoveTask { selector: ".".to_string(), name: "dev".to_string() },
        Request::RunTask { selector: ".".to_string(), name: "dev".to_string() },
        Request::ControlSupervised {
            name: "flip7".to_string(),
            action: "restart".to_string(),
        },
        Request::AdoptPort { port: 3007, force: true },
        Request::StartService {
            project: None,
            service: "web".to_string(),
            port: Some(3000),
            on_conflict: Some("fail".to_string()),
            started_by: Some("cli".to_string()),
            session: None,
        },
        Request::StopService { project: None, service: "web".to_string(), timeout_seconds: Some(5) },
        Request::RestartService {
            project: None,
            service: "web".to_string(),
            started_by: None,
            session: None,
        },
        Request::ControlContainer { name: "db".to_string(), action: "stop".to_string() },
        Request::GetContainerLogs { name: "db".to_string(), max_lines: Some(10) },
        Request::GetHealth { project: None, service: "web".to_string() },
        Request::WaitUntilHealthy { project: None, service: "web".to_string(), timeout_seconds: None },
        Request::CheckPort { port: 3000 },
        Request::ListPorts,
        Request::ReservePort {
            project: None,
            service: "web".to_string(),
            port: None,
            on_conflict: None,
            started_by: None,
        },
        Request::ReleasePort { port: 3000 },
        Request::GetLogs {
            project: None,
            service: "web".to_string(),
            max_lines: Some(50),
            since_seq: Some(3),
        },
        Request::GetSetting { key: "desktop.panel".to_string() },
        Request::SetSetting { key: "k".to_string(), value: "v".to_string() },
        Request::ListSessions,
        Request::RegisterSession {
            provider: "anthropic".to_string(),
            client: "claude-code".to_string(),
            cwd: None,
        },
        Request::Subscribe,
    ];

    for request in requests {
        let label = format!("{request:?}");
        let frame = Frame::Request { id: 1, request };

        let encoded = serde_json::to_string(&frame)
            .unwrap_or_else(|err| panic!("cannot encode {label}: {err}"));
        let decoded: Frame = serde_json::from_str(&encoded)
            .unwrap_or_else(|err| panic!("cannot decode {label}: {err}\n{encoded}"));
        assert_eq!(decoded, frame, "changed shape: {label}");
    }
}
