//! SQLite persistence.
//!
//! The database is the authority for *declarations* — projects, workspaces,
//! services, leases. It is never the authority for whether something is
//! running: that is reconciled against the OS on every daemon start.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Row};
use runtime_types::{
    AgentSession, HealthCheck, InstanceId, PortLease, PortLeaseStatus, Project, ProjectId, Result,
    RuntimeError, RuntimeInstance, Service, ServiceId, ServiceStatus, SessionId, StartedBy,
    Stack, StackId, Workspace, WorkspaceId,
};

const SCHEMA_VERSION: i64 = 2;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS projects (
    id             TEXT PRIMARY KEY,
    name           TEXT NOT NULL,
    root_path      TEXT NOT NULL UNIQUE,
    repository_url TEXT,
    created_at     TEXT NOT NULL,
    updated_at     TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS workspaces (
    id          TEXT PRIMARY KEY,
    project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    path        TEXT NOT NULL UNIQUE,
    git_branch  TEXT,
    git_commit  TEXT,
    worktree    INTEGER NOT NULL DEFAULT 0,
    port_offset INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_workspaces_project ON workspaces(project_id);

CREATE TABLE IF NOT EXISTS services (
    id              TEXT PRIMARY KEY,
    workspace_id    TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    name            TEXT NOT NULL,
    service_type    TEXT NOT NULL,
    command         TEXT NOT NULL,
    cwd             TEXT NOT NULL,
    env             TEXT NOT NULL DEFAULT '{}',
    preferred_port  INTEGER,
    health_check    TEXT,
    auto_start      INTEGER NOT NULL DEFAULT 0,
    conflict_policy TEXT NOT NULL DEFAULT 'allocate-next',
    depends_on      TEXT NOT NULL DEFAULT '[]',
    one_shot        INTEGER NOT NULL DEFAULT 0,
    UNIQUE(workspace_id, name)
);

CREATE TABLE IF NOT EXISTS stacks (
    id           TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    members      TEXT NOT NULL DEFAULT '[]',
    UNIQUE(workspace_id, name)
);
CREATE INDEX IF NOT EXISTS idx_stacks_workspace ON stacks(workspace_id);
CREATE INDEX IF NOT EXISTS idx_services_workspace ON services(workspace_id);

CREATE TABLE IF NOT EXISTS instances (
    id                 TEXT PRIMARY KEY,
    service_id         TEXT NOT NULL REFERENCES services(id) ON DELETE CASCADE,
    pid                INTEGER NOT NULL,
    process_start_time INTEGER NOT NULL,
    status             TEXT NOT NULL,
    port               INTEGER,
    started_at         TEXT NOT NULL,
    stopped_at         TEXT,
    exit_code          INTEGER,
    started_by         TEXT NOT NULL DEFAULT 'unknown',
    owner_session      TEXT
);
CREATE INDEX IF NOT EXISTS idx_instances_service ON instances(service_id, started_at DESC);

CREATE TABLE IF NOT EXISTS port_leases (
    port         INTEGER PRIMARY KEY,
    project_id   TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    service_id   TEXT NOT NULL REFERENCES services(id) ON DELETE CASCADE,
    preferred    INTEGER NOT NULL DEFAULT 0,
    status       TEXT NOT NULL,
    owner        TEXT NOT NULL DEFAULT 'unknown',
    created_at   TEXT NOT NULL,
    expires_at   TEXT
);
CREATE INDEX IF NOT EXISTS idx_leases_service ON port_leases(service_id);

CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS agent_sessions (
    id           TEXT PRIMARY KEY,
    provider     TEXT NOT NULL,
    client       TEXT NOT NULL,
    cwd          TEXT,
    project_id   TEXT,
    started_at   TEXT NOT NULL,
    last_seen_at TEXT NOT NULL
);
"#;

pub struct Store {
    conn: Mutex<Connection>,
    path: PathBuf,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store").field("path", &self.path).finish()
    }
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(&path).map_err(sqlite_err)?;
        Self::init(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            path,
        })
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().map_err(sqlite_err)?;
        Self::init(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            path: PathBuf::from(":memory:"),
        })
    }

    fn init(conn: &Connection) -> Result<()> {
        // WAL keeps the daemon writing while the CLI and GUI read.
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(sqlite_err)?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(sqlite_err)?;
        // After the schema, not before: the copy needs the new table to exist,
        // and `CREATE TABLE IF NOT EXISTS` leaves a populated one alone.
        conn.execute_batch(SCHEMA).map_err(sqlite_err)?;
        Self::carry_over_old_tasks(conn);
        Self::add_missing_columns(conn);
        conn.execute(
            "INSERT INTO meta(key, value) VALUES('schema_version', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![SCHEMA_VERSION.to_string()],
        )
        .map_err(sqlite_err)?;
        Ok(())
    }

    /// A stack used to be called a task, in the database as well.
    ///
    /// Carried over rather than abandoned: somebody's declared stacks are the
    /// one thing here typed by hand rather than detected, and `CREATE TABLE IF
    /// NOT EXISTS stacks` beside the old table would leave them in place and
    /// invisible.
    ///
    /// Copies rather than renames, and copies row by row, because the first
    /// version of this shipped broken — a rename sweep rewrote its own literals
    /// so the guard read `has(new) && !has(new)`, which is never true. It did
    /// nothing, silently, and the schema then created the new table empty
    /// beside the full old one. So this has to handle that state too, and the
    /// way to handle both is to ask what is missing rather than what happened.
    fn carry_over_old_tasks(conn: &Connection) {
        let old = "tasks";
        let exists = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
                params![old],
                |_| Ok(()),
            )
            .is_ok();
        if !exists {
            return;
        }
        // Only what the new table does not already have, so running twice is
        // the same as running once.
        let copy = format!(
            "INSERT INTO stacks(id, workspace_id, name, members)
             SELECT id, workspace_id, name, steps FROM {old}
             WHERE NOT EXISTS (
                 SELECT 1 FROM stacks s
                 WHERE s.workspace_id = {old}.workspace_id AND s.name = {old}.name
             )"
        );
        if conn.execute(&copy, []).is_ok() {
            let _ = conn.execute(&format!("DROP TABLE {old}"), []);
        }
    }

    /// Bring an existing database up to the current shape.
    ///
    /// `CREATE TABLE IF NOT EXISTS` leaves an older table exactly as it was, so
    /// a column added to the schema never reaches a database that already
    /// exists — and the failure is at query time, on a machine with real data
    /// in it. Each of these is additive with a default, so running them on a
    /// database that already has the column is a no-op and its error is the
    /// expected outcome rather than a problem.
    fn add_missing_columns(conn: &Connection) {
        const ADDITIONS: &[&str] = &[
            "ALTER TABLE services ADD COLUMN depends_on TEXT NOT NULL DEFAULT '[]'",
            "ALTER TABLE services ADD COLUMN one_shot INTEGER NOT NULL DEFAULT 0",
        ];
        for statement in ADDITIONS {
            // "duplicate column name" is the ordinary case: the column is
            // already there because the database was created from the current
            // schema, or a previous start added it.
            let _ = conn.execute(statement, []);
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn with_conn<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let guard = self
            .conn
            .lock()
            .map_err(|_| RuntimeError::internal("database lock poisoned"))?;
        f(&guard)
    }

    // ---- projects ------------------------------------------------------

    pub fn upsert_project(&self, project: &Project) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO projects(id, name, root_path, repository_url, created_at, updated_at)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(root_path) DO UPDATE SET
                     name = excluded.name,
                     repository_url = excluded.repository_url,
                     updated_at = excluded.updated_at",
                params![
                    project.id.as_str(),
                    project.name,
                    path_str(&project.root_path),
                    project.repository_url,
                    ts(project.created_at),
                    ts(project.updated_at),
                ],
            )
            .map_err(sqlite_err)?;
            Ok(())
        })
    }

    pub fn list_projects(&self) -> Result<Vec<Project>> {
        self.with_conn(|conn| {
            let mut stmt = conn
                .prepare("SELECT * FROM projects ORDER BY name COLLATE NOCASE")
                .map_err(sqlite_err)?;
            let rows = stmt
                .query_map([], |row| Ok(row_project(row)))
                .map_err(sqlite_err)?;
            collect(rows)
        })
    }

    pub fn get_project(&self, id: &ProjectId) -> Result<Option<Project>> {
        self.with_conn(|conn| {
            conn.query_row("SELECT * FROM projects WHERE id = ?1", params![id.as_str()], |row| {
                Ok(row_project(row))
            })
            .optional()
            .map_err(sqlite_err)?
            .transpose()
        })
    }

    pub fn find_project_by_path(&self, path: &Path) -> Result<Option<Project>> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT * FROM projects WHERE root_path = ?1",
                params![path_str(path)],
                |row| Ok(row_project(row)),
            )
            .optional()
            .map_err(sqlite_err)?
            .transpose()
        })
    }

    pub fn delete_project(&self, id: &ProjectId) -> Result<bool> {
        self.with_conn(|conn| {
            let changed = conn
                .execute("DELETE FROM projects WHERE id = ?1", params![id.as_str()])
                .map_err(sqlite_err)?;
            Ok(changed > 0)
        })
    }

    // ---- workspaces ----------------------------------------------------

    pub fn upsert_workspace(&self, workspace: &Workspace) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO workspaces(id, project_id, path, git_branch, git_commit, worktree, port_offset, created_at)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(path) DO UPDATE SET
                     git_branch = excluded.git_branch,
                     git_commit = excluded.git_commit,
                     worktree = excluded.worktree",
                params![
                    workspace.id.as_str(),
                    workspace.project_id.as_str(),
                    path_str(&workspace.path),
                    workspace.git_branch,
                    workspace.git_commit,
                    workspace.worktree as i64,
                    workspace.port_offset as i64,
                    ts(workspace.created_at),
                ],
            )
            .map_err(sqlite_err)?;
            Ok(())
        })
    }

    /// Every checkout the runtime knows, across all projects.
    ///
    /// For resolving a path to whatever owns it: a git worktree lives outside
    /// the repository it was branched from, so the only way to match one is to
    /// look at checkouts rather than project roots.
    pub fn list_workspaces_all(&self) -> Result<Vec<Workspace>> {
        self.with_conn(|conn| {
            let mut statement = conn
                .prepare("SELECT * FROM workspaces")
                .map_err(sqlite_err)?;
            let rows = statement
                .query_map([], |row| Ok(row_workspace(row)))
                .map_err(sqlite_err)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row.map_err(sqlite_err)??);
            }
            Ok(out)
        })
    }

    pub fn list_workspaces(&self, project_id: &ProjectId) -> Result<Vec<Workspace>> {
        self.with_conn(|conn| {
            let mut stmt = conn
                .prepare("SELECT * FROM workspaces WHERE project_id = ?1 ORDER BY port_offset")
                .map_err(sqlite_err)?;
            let rows = stmt
                .query_map(params![project_id.as_str()], |row| Ok(row_workspace(row)))
                .map_err(sqlite_err)?;
            collect(rows)
        })
    }

    pub fn get_workspace(&self, id: &WorkspaceId) -> Result<Option<Workspace>> {
        self.with_conn(|conn| {
            conn.query_row("SELECT * FROM workspaces WHERE id = ?1", params![id.as_str()], |row| {
                Ok(row_workspace(row))
            })
            .optional()
            .map_err(sqlite_err)?
            .transpose()
        })
    }

    pub fn find_workspace_by_path(&self, path: &Path) -> Result<Option<Workspace>> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT * FROM workspaces WHERE path = ?1",
                params![path_str(path)],
                |row| Ok(row_workspace(row)),
            )
            .optional()
            .map_err(sqlite_err)?
            .transpose()
        })
    }

    /// Every workspace, used when resolving an arbitrary pid's cwd to a project.
    pub fn all_workspaces(&self) -> Result<Vec<Workspace>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT * FROM workspaces").map_err(sqlite_err)?;
            let rows = stmt
                .query_map([], |row| Ok(row_workspace(row)))
                .map_err(sqlite_err)?;
            collect(rows)
        })
    }

    /// The lowest offset not yet taken by a workspace of this project.
    pub fn next_port_offset(&self, project_id: &ProjectId) -> Result<u16> {
        let used: Vec<u16> = self
            .list_workspaces(project_id)?
            .into_iter()
            .map(|w| w.port_offset)
            .collect();
        Ok((0u16..u16::MAX).find(|n| !used.contains(n)).unwrap_or(0))
    }

    // ---- stacks --------------------------------------------------------

    pub fn upsert_stack(&self, stack: &Stack) -> Result<()> {
        let members = serde_json::to_string(&stack.members).map_err(json_err)?;
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO stacks(id, workspace_id, name, members)
                 VALUES(?1, ?2, ?3, ?4)
                 ON CONFLICT(workspace_id, name) DO UPDATE SET members = excluded.members",
                params![
                    stack.id.as_str(),
                    stack.workspace_id.as_str(),
                    stack.name,
                    members
                ],
            )
            .map_err(sqlite_err)?;
            Ok(())
        })
    }

    pub fn list_stacks(&self, workspace_id: &WorkspaceId) -> Result<Vec<Stack>> {
        self.with_conn(|conn| {
            let mut statement = conn
                .prepare("SELECT * FROM stacks WHERE workspace_id = ?1 ORDER BY name")
                .map_err(sqlite_err)?;
            let rows = statement
                .query_map(params![workspace_id.as_str()], |row| Ok(row_task(row)))
                .map_err(sqlite_err)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row.map_err(sqlite_err)??);
            }
            Ok(out)
        })
    }

    pub fn remove_stack(&self, id: &StackId) -> Result<bool> {
        self.with_conn(|conn| {
            let changed = conn
                .execute("DELETE FROM stacks WHERE id = ?1", params![id.as_str()])
                .map_err(sqlite_err)?;
            Ok(changed > 0)
        })
    }

    // ---- services ------------------------------------------------------

    pub fn upsert_service(&self, service: &Service) -> Result<()> {
        let env = serde_json::to_string(&service.env).map_err(json_err)?;
        let depends_on = serde_json::to_string(&service.depends_on).map_err(json_err)?;
        let health = service
            .health_check
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(json_err)?;
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO services(id, workspace_id, name, service_type, command, cwd, env,
                                      preferred_port, health_check, auto_start, conflict_policy,
                                      depends_on, one_shot)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                 ON CONFLICT(workspace_id, name) DO UPDATE SET
                     service_type = excluded.service_type,
                     command = excluded.command,
                     cwd = excluded.cwd,
                     env = excluded.env,
                     preferred_port = excluded.preferred_port,
                     health_check = excluded.health_check,
                     auto_start = excluded.auto_start,
                     conflict_policy = excluded.conflict_policy,
                     depends_on = excluded.depends_on,
                     one_shot = excluded.one_shot",
                params![
                    service.id.as_str(),
                    service.workspace_id.as_str(),
                    service.name,
                    json_tag(service.service_type),
                    service.command,
                    path_str(&service.cwd),
                    env,
                    service.preferred_port.map(|p| p as i64),
                    health,
                    service.auto_start as i64,
                    json_tag(service.conflict_policy),
                    depends_on,
                    service.one_shot as i64,
                ],
            )
            .map_err(sqlite_err)?;
            Ok(())
        })
    }

    pub fn get_service(&self, id: &ServiceId) -> Result<Option<Service>> {
        self.with_conn(|conn| {
            conn.query_row("SELECT * FROM services WHERE id = ?1", params![id.as_str()], |row| {
                Ok(row_service(row))
            })
            .optional()
            .map_err(sqlite_err)?
            .transpose()
        })
    }

    pub fn list_services(&self, workspace_id: &WorkspaceId) -> Result<Vec<Service>> {
        self.with_conn(|conn| {
            let mut stmt = conn
                .prepare("SELECT * FROM services WHERE workspace_id = ?1 ORDER BY name COLLATE NOCASE")
                .map_err(sqlite_err)?;
            let rows = stmt
                .query_map(params![workspace_id.as_str()], |row| Ok(row_service(row)))
                .map_err(sqlite_err)?;
            collect(rows)
        })
    }

    pub fn all_services(&self) -> Result<Vec<Service>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT * FROM services").map_err(sqlite_err)?;
            let rows = stmt
                .query_map([], |row| Ok(row_service(row)))
                .map_err(sqlite_err)?;
            collect(rows)
        })
    }

    pub fn delete_service(&self, id: &ServiceId) -> Result<bool> {
        self.with_conn(|conn| {
            let changed = conn
                .execute("DELETE FROM services WHERE id = ?1", params![id.as_str()])
                .map_err(sqlite_err)?;
            Ok(changed > 0)
        })
    }

    /// Forget a checkout. Its services and stacks go with it, by cascade.
    pub fn delete_workspace(&self, id: &WorkspaceId) -> Result<bool> {
        self.with_conn(|conn| {
            let changed = conn
                .execute("DELETE FROM workspaces WHERE id = ?1", params![id.as_str()])
                .map_err(sqlite_err)?;
            Ok(changed > 0)
        })
    }

    // ---- instances -----------------------------------------------------

    pub fn insert_instance(&self, instance: &RuntimeInstance) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO instances(id, service_id, pid, process_start_time, status, port,
                                       started_at, stopped_at, exit_code, started_by, owner_session)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    instance.id.as_str(),
                    instance.service_id.as_str(),
                    instance.pid as i64,
                    instance.process_start_time,
                    json_tag(instance.status),
                    instance.port.map(|p| p as i64),
                    ts(instance.started_at),
                    instance.stopped_at.map(ts),
                    instance.exit_code,
                    json_tag(instance.started_by),
                    instance.owner_session.as_ref().map(|s| s.0.clone()),
                ],
            )
            .map_err(sqlite_err)?;
            Ok(())
        })
    }

    pub fn update_instance(&self, instance: &RuntimeInstance) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "UPDATE instances SET status = ?2, port = ?3, stopped_at = ?4, exit_code = ?5,
                                      pid = ?6, process_start_time = ?7
                 WHERE id = ?1",
                params![
                    instance.id.as_str(),
                    json_tag(instance.status),
                    instance.port.map(|p| p as i64),
                    instance.stopped_at.map(ts),
                    instance.exit_code,
                    instance.pid as i64,
                    instance.process_start_time,
                ],
            )
            .map_err(sqlite_err)?;
            Ok(())
        })
    }

    /// The most recent instance for a service, running or not.
    pub fn latest_instance(&self, service_id: &ServiceId) -> Result<Option<RuntimeInstance>> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT * FROM instances WHERE service_id = ?1 ORDER BY started_at DESC LIMIT 1",
                params![service_id.as_str()],
                |row| Ok(row_instance(row)),
            )
            .optional()
            .map_err(sqlite_err)?
            .transpose()
        })
    }

    /// Every instance the database still believes is live.
    pub fn live_instances(&self) -> Result<Vec<RuntimeInstance>> {
        self.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT * FROM instances
                     WHERE status IN ('starting', 'healthy', 'unhealthy', 'stopping')",
                )
                .map_err(sqlite_err)?;
            let rows = stmt
                .query_map([], |row| Ok(row_instance(row)))
                .map_err(sqlite_err)?;
            collect(rows)
        })
    }

    pub fn get_instance(&self, id: &InstanceId) -> Result<Option<RuntimeInstance>> {
        self.with_conn(|conn| {
            conn.query_row("SELECT * FROM instances WHERE id = ?1", params![id.as_str()], |row| {
                Ok(row_instance(row))
            })
            .optional()
            .map_err(sqlite_err)?
            .transpose()
        })
    }

    // ---- port leases ---------------------------------------------------

    pub fn upsert_lease(&self, lease: &PortLease) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO port_leases(port, project_id, workspace_id, service_id, preferred,
                                         status, owner, created_at, expires_at)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(port) DO UPDATE SET
                     project_id = excluded.project_id,
                     workspace_id = excluded.workspace_id,
                     service_id = excluded.service_id,
                     preferred = excluded.preferred,
                     status = excluded.status,
                     owner = excluded.owner,
                     expires_at = excluded.expires_at",
                params![
                    lease.port as i64,
                    lease.project_id.as_str(),
                    lease.workspace_id.as_str(),
                    lease.service_id.as_str(),
                    lease.preferred as i64,
                    json_tag(lease.status),
                    json_tag(lease.owner),
                    ts(lease.created_at),
                    lease.expires_at.map(ts),
                ],
            )
            .map_err(sqlite_err)?;
            Ok(())
        })
    }

    pub fn get_lease(&self, port: u16) -> Result<Option<PortLease>> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT * FROM port_leases WHERE port = ?1",
                params![port as i64],
                |row| Ok(row_lease(row)),
            )
            .optional()
            .map_err(sqlite_err)?
            .transpose()
        })
    }

    pub fn list_leases(&self) -> Result<Vec<PortLease>> {
        self.with_conn(|conn| {
            let mut stmt = conn
                .prepare("SELECT * FROM port_leases ORDER BY port")
                .map_err(sqlite_err)?;
            let rows = stmt
                .query_map([], |row| Ok(row_lease(row)))
                .map_err(sqlite_err)?;
            collect(rows)
        })
    }

    pub fn release_lease(&self, port: u16) -> Result<bool> {
        self.with_conn(|conn| {
            let changed = conn
                .execute("DELETE FROM port_leases WHERE port = ?1", params![port as i64])
                .map_err(sqlite_err)?;
            Ok(changed > 0)
        })
    }

    pub fn release_leases_for_service(&self, service_id: &ServiceId) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "DELETE FROM port_leases WHERE service_id = ?1",
                params![service_id.as_str()],
            )
            .map_err(sqlite_err)?;
            Ok(())
        })
    }

    /// Drops reservations whose holder never started anything.
    pub fn expire_leases(&self, now: DateTime<Utc>) -> Result<usize> {
        self.with_conn(|conn| {
            let removed = conn
                .execute(
                    "DELETE FROM port_leases
                     WHERE status = 'reserved' AND expires_at IS NOT NULL AND expires_at < ?1",
                    params![ts(now)],
                )
                .map_err(sqlite_err)?;
            Ok(removed)
        })
    }

    // ---- settings ------------------------------------------------------
    //
    // Opaque strings on purpose. The panel's geometry means nothing to the
    // daemon, but keeping it here is what makes it survive reinstalling the
    // app, and gives one answer to "where is the state" rather than two.

    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(sqlite_err)
        })
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO settings(key, value) VALUES(?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
            .map_err(sqlite_err)?;
            Ok(())
        })
    }

    // ---- agent sessions ------------------------------------------------

    pub fn upsert_session(&self, session: &AgentSession) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO agent_sessions(id, provider, client, cwd, project_id, started_at, last_seen_at)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(id) DO UPDATE SET
                     cwd = excluded.cwd,
                     project_id = excluded.project_id,
                     last_seen_at = excluded.last_seen_at",
                params![
                    session.id.as_str(),
                    session.provider,
                    session.client,
                    session.cwd.as_deref().map(path_str),
                    session.project_id.as_ref().map(|p| p.0.clone()),
                    ts(session.started_at),
                    ts(session.last_seen_at),
                ],
            )
            .map_err(sqlite_err)?;
            Ok(())
        })
    }

    pub fn get_session(&self, id: &SessionId) -> Result<Option<AgentSession>> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT * FROM agent_sessions WHERE id = ?1",
                params![id.as_str()],
                |row| Ok(row_session(row)),
            )
            .optional()
            .map_err(sqlite_err)?
            .transpose()
        })
    }

    pub fn list_sessions(&self) -> Result<Vec<AgentSession>> {
        self.with_conn(|conn| {
            let mut stmt = conn
                .prepare("SELECT * FROM agent_sessions ORDER BY last_seen_at DESC")
                .map_err(sqlite_err)?;
            let rows = stmt
                .query_map([], |row| Ok(row_session(row)))
                .map_err(sqlite_err)?;
            collect(rows)
        })
    }
}

// ---- row mapping -------------------------------------------------------
//
// `query_map` closures return `rusqlite::Result`, but our conversions can fail
// for reasons SQLite knows nothing about (bad JSON, unknown enum tag). Each
// mapper therefore returns `Result<T>` nested inside the rusqlite result, and
// `collect` flattens the two layers.

fn collect<T>(
    rows: rusqlite::MappedRows<'_, impl FnMut(&Row<'_>) -> rusqlite::Result<Result<T>>>,
) -> Result<Vec<T>> {
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(sqlite_err)??);
    }
    Ok(out)
}

fn row_project(row: &Row<'_>) -> Result<Project> {
    Ok(Project {
        id: ProjectId(get_text(row, "id")?),
        name: get_text(row, "name")?,
        root_path: PathBuf::from(get_text(row, "root_path")?),
        repository_url: get_opt_text(row, "repository_url")?,
        created_at: parse_ts(&get_text(row, "created_at")?)?,
        updated_at: parse_ts(&get_text(row, "updated_at")?)?,
    })
}

fn row_workspace(row: &Row<'_>) -> Result<Workspace> {
    Ok(Workspace {
        id: WorkspaceId(get_text(row, "id")?),
        project_id: ProjectId(get_text(row, "project_id")?),
        path: PathBuf::from(get_text(row, "path")?),
        git_branch: get_opt_text(row, "git_branch")?,
        git_commit: get_opt_text(row, "git_commit")?,
        worktree: get_int(row, "worktree")? != 0,
        port_offset: get_int(row, "port_offset")? as u16,
        created_at: parse_ts(&get_text(row, "created_at")?)?,
    })
}

fn row_service(row: &Row<'_>) -> Result<Service> {
    let env: BTreeMap<String, String> =
        serde_json::from_str(&get_text(row, "env")?).map_err(json_err)?;
    let health_check: Option<HealthCheck> = match get_opt_text(row, "health_check")? {
        Some(raw) => Some(serde_json::from_str(&raw).map_err(json_err)?),
        None => None,
    };
    Ok(Service {
        id: ServiceId(get_text(row, "id")?),
        workspace_id: WorkspaceId(get_text(row, "workspace_id")?),
        name: get_text(row, "name")?,
        service_type: parse_tag(&get_text(row, "service_type")?)?,
        command: get_text(row, "command")?,
        cwd: PathBuf::from(get_text(row, "cwd")?),
        env,
        preferred_port: get_opt_int(row, "preferred_port")?.map(|v| v as u16),
        health_check,
        auto_start: get_int(row, "auto_start")? != 0,
        conflict_policy: parse_tag(&get_text(row, "conflict_policy")?)?,
        // Tolerated as absent: a row written before the column existed reads
        // back through the default, and a database that predates the migration
        // should open rather than fail.
        depends_on: get_opt_text(row, "depends_on")?
            .map(|raw| serde_json::from_str(&raw).unwrap_or_default())
            .unwrap_or_default(),
        one_shot: get_opt_int(row, "one_shot")?.unwrap_or(0) != 0,
    })
}

fn row_task(row: &Row<'_>) -> Result<Stack> {
    Ok(Stack {
        id: StackId(get_text(row, "id")?),
        workspace_id: WorkspaceId(get_text(row, "workspace_id")?),
        name: get_text(row, "name")?,
        members: serde_json::from_str(&get_text(row, "members")?).map_err(json_err)?,
    })
}

fn row_instance(row: &Row<'_>) -> Result<RuntimeInstance> {
    Ok(RuntimeInstance {
        id: InstanceId(get_text(row, "id")?),
        service_id: ServiceId(get_text(row, "service_id")?),
        pid: get_int(row, "pid")? as u32,
        process_start_time: get_int(row, "process_start_time")?,
        status: parse_tag::<ServiceStatus>(&get_text(row, "status")?)?,
        port: get_opt_int(row, "port")?.map(|v| v as u16),
        started_at: parse_ts(&get_text(row, "started_at")?)?,
        stopped_at: get_opt_text(row, "stopped_at")?
            .map(|raw| parse_ts(&raw))
            .transpose()?,
        exit_code: get_opt_int(row, "exit_code")?.map(|v| v as i32),
        started_by: parse_tag(&get_text(row, "started_by")?)?,
        owner_session: get_opt_text(row, "owner_session")?.map(SessionId),
    })
}

fn row_lease(row: &Row<'_>) -> Result<PortLease> {
    Ok(PortLease {
        port: get_int(row, "port")? as u16,
        project_id: ProjectId(get_text(row, "project_id")?),
        workspace_id: WorkspaceId(get_text(row, "workspace_id")?),
        service_id: ServiceId(get_text(row, "service_id")?),
        preferred: get_int(row, "preferred")? != 0,
        status: parse_tag::<PortLeaseStatus>(&get_text(row, "status")?)?,
        owner: parse_tag::<StartedBy>(&get_text(row, "owner")?)?,
        created_at: parse_ts(&get_text(row, "created_at")?)?,
        expires_at: get_opt_text(row, "expires_at")?
            .map(|raw| parse_ts(&raw))
            .transpose()?,
    })
}

fn row_session(row: &Row<'_>) -> Result<AgentSession> {
    Ok(AgentSession {
        id: SessionId(get_text(row, "id")?),
        provider: get_text(row, "provider")?,
        client: get_text(row, "client")?,
        cwd: get_opt_text(row, "cwd")?.map(PathBuf::from),
        project_id: get_opt_text(row, "project_id")?.map(ProjectId),
        started_at: parse_ts(&get_text(row, "started_at")?)?,
        last_seen_at: parse_ts(&get_text(row, "last_seen_at")?)?,
    })
}

// ---- column helpers ----------------------------------------------------

fn get_text(row: &Row<'_>, column: &str) -> Result<String> {
    row.get::<_, String>(column).map_err(sqlite_err)
}

fn get_opt_text(row: &Row<'_>, column: &str) -> Result<Option<String>> {
    row.get::<_, Option<String>>(column).map_err(sqlite_err)
}

fn get_int(row: &Row<'_>, column: &str) -> Result<i64> {
    row.get::<_, i64>(column).map_err(sqlite_err)
}

fn get_opt_int(row: &Row<'_>, column: &str) -> Result<Option<i64>> {
    row.get::<_, Option<i64>>(column).map_err(sqlite_err)
}

/// Enums are stored using their serde representation, so the on-disk value and
/// the value on the wire are always identical.
fn json_tag<T: serde::Serialize>(value: T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string())
}

fn parse_tag<T: serde::de::DeserializeOwned>(raw: &str) -> Result<T> {
    serde_json::from_value(serde_json::Value::String(raw.to_string()))
        .map_err(|_| RuntimeError::internal(format!("unknown stored value '{raw}'")))
}

fn ts(value: DateTime<Utc>) -> String {
    value.to_rfc3339()
}

fn parse_ts(raw: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|err| RuntimeError::internal(format!("bad timestamp '{raw}': {err}")))
}

fn path_str(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn sqlite_err(err: rusqlite::Error) -> RuntimeError {
    RuntimeError::internal(format!("sqlite: {err}"))
}

fn json_err(err: serde_json::Error) -> RuntimeError {
    RuntimeError::internal(format!("json: {err}"))
}
