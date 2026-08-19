# Local Dev Runtime Manager — 跨平台实现计划

## 1. 项目定位

### 1.1 一句话定义

一个面向开发者与 AI Coding Agent 的跨平台本地开发运行时管理器，用统一的 GUI、CLI 与 MCP 接口管理本机项目、开发服务、端口、日志、Git Worktree 与运行状态。

目标平台：

- macOS
- Windows
- Linux 作为后续扩展，不进入第一阶段硬性范围

### 1.2 不做什么

本项目不定位为传统的：

- `lsof` GUI
- 单纯端口查看器
- 单纯进程 Kill 工具
- 单纯 Dev Server Launcher
- 单纯 Claude Code MCP Wrapper

端口只是运行时状态中的一个字段。

产品核心对象应是：

```text
Project
  └── Workspace / Worktree
        └── Service
              ├── Process
              ├── Port
              ├── Health
              ├── Logs
              └── Agent Ownership
```

### 1.3 核心价值

解决本地 AI 辅助开发逐渐出现的几个问题：

1. 多个项目同时启动时，不知道某个 localhost 端口属于谁。
2. Claude Code、Codex、Cursor、Terminal 等可能同时启动服务。
3. Git Worktree 会产生大量同项目、不同 branch 的运行实例。
4. Agent 自己使用 shell 启停服务时缺少统一状态。
5. 端口冲突通常在启动后才发现。
6. Dev Server 的 PID、工作目录、启动命令、日志分散在不同终端。
7. 人类 GUI 和 Agent 自动化目前通常使用两套独立工具。

本项目希望成为：

> Local Development Runtime Control Plane

---

# 2. 产品形态

## 2.1 四个访问入口

所有入口访问同一个 Runtime Core。

```text
                     Runtime Core
                          │
         ┌────────────────┼────────────────┐
         │                │                │
       Desktop           CLI              MCP
         │                │                │
  Sidebar / Tray      Terminal       Coding Agents
```

第四个可选入口：

```text
Local HTTP / IPC API
```

主要用于插件、IDE 或未来其他客户端。

---

# 3. GUI 设计

## 3.1 主界面

主窗口按 Project 而不是 Port 展示。

```text
┌─────────────────────────────────────────────┐
│ Local Runtime                               │
├───────────────────┬─────────────────────────┤
│ Projects          │ DOSSH                   │
│                   │                         │
│ ● DOSSH           │ branch: feature/refund  │
│   4 services      │ agent: Claude Code      │
│                   │                         │
│ ● Loom            │ ● Web        :3000      │
│   2 services      │ ● API        :8000      │
│                   │ ● Postgres   :5432      │
│ ○ Flinders        │ ● Redis      :6379      │
│                   │                         │
│                   │ [Open] [Restart] [Stop] │
│                   │                         │
│                   │ Logs                    │
│                   │ GET /api/...            │
└───────────────────┴─────────────────────────┘
```

## 3.2 Side Panel

核心差异化交互。

支持：

- 固定在屏幕左侧或右侧
- Always-on-top
- 自动隐藏
- 鼠标靠近展开
- 可调整宽度
- 紧凑模式
- 多显示器
- 可选择固定显示器
- 不抢焦点模式

紧凑状态：

```text
│ ● DOSSH
│ ● Loom
│ ○ Flinders
│
│ + New
```

展开状态：

```text
DOSSH
● Web       :3000
● API       :8000
● DB        :5432

LOOM
● Web       :3001
● Worker

[ Start All ]
```

## 3.3 Tray / Menu Bar

macOS：

- Menu Bar Item

Windows：

- System Tray

主要执行轻量操作：

- 查看 Running Projects
- Start / Stop
- Open URL
- Copy URL
- Open Logs
- Open Main Window

---

# 4. 跨平台技术架构

## 4.1 推荐技术栈

### Desktop Shell

**Tauri 2**

前端：

- React
- TypeScript
- Vite

原因：

- macOS / Windows 双平台
- 比 Electron 更轻
- Rust 后端适合进程管理
- 可以调用原生 API
- CLI / Core 可以与桌面端共享 Rust crate
- 后续 Linux 扩展成本相对可控

### Runtime Core

Rust。

建议拆成独立 crate：

```text
runtime-core
```

负责：

- Project Registry
- Service Registry
- Process Lifecycle
- Port Allocation
- Process Discovery
- Health Check
- Log Streaming
- Worktree Detection
- Ownership
- State Persistence
- Event Bus

### MCP

建议第一阶段使用：

**TypeScript MCP Server + Local Runtime API**

而不是把 MCP 协议直接写进 GUI。

结构：

```text
Claude Code
    │
    │ stdio MCP
    ▼
runtime-mcp
    │
    │ local IPC / localhost API
    ▼
runtime-daemon
```

优势：

- MCP SDK 迭代与 Rust Core 解耦
- Claude Code 配置简单
- GUI 关闭后 MCP 仍可工作
- MCP Crash 不影响 Runtime
- 后续可以独立发布 MCP package

后期可视情况迁移为 Rust MCP Server。

---

# 5. Runtime 架构

## 5.1 推荐进程模型

不要让 Desktop GUI 本身成为唯一后台进程。

建议：

```text
                 ┌─────────────┐
                 │ Desktop App │
                 └──────┬──────┘
                        │
                        ▼
┌─────────┐       ┌───────────────┐       ┌─────────┐
│   CLI   │ ────► │ Runtime Daemon│ ◄──── │   MCP   │
└─────────┘       └───────┬───────┘       └─────────┘
                          │
                          ▼
                  OS Adapter Layer
```

Runtime Daemon 是唯一状态权威。

这样：

- GUI 重启不会丢 Service 状态
- Agent 可在 GUI 未打开时调用
- CLI 与 GUI 不会互相覆盖状态
- 多个 MCP Client 可安全共享 Runtime

---

# 6. 核心数据模型

## 6.1 Project

```text
Project
- id
- name
- rootPath
- repositoryUrl
- createdAt
- updatedAt
```

## 6.2 Workspace

用于 Git Branch / Worktree。

```text
Workspace
- id
- projectId
- path
- gitBranch
- gitCommit
- worktree
```

## 6.3 Service

```text
Service
- id
- workspaceId
- name
- type
- command
- cwd
- env
- preferredPort
- actualPort
- healthCheck
- autoStart
```

Service type：

```text
web
api
worker
database
cache
container
custom
```

## 6.4 Runtime Instance

```text
RuntimeInstance
- id
- serviceId
- pid
- processStartTime
- status
- startedAt
- stoppedAt
- startedBy
- ownerSession
```

`startedBy`：

```text
manual
desktop
cli
claude-code
codex
cursor
unknown
```

## 6.5 Port Lease

不要只记录“哪个 PID 在哪个 Port”。

维护真正的 Port Lease：

```text
PortLease
- port
- projectId
- workspaceId
- serviceId
- preferred
- status
- owner
- expiresAt
```

状态：

```text
reserved
active
released
conflicted
```

---

# 7. Port Management

## 7.1 Port Discovery

需要识别：

```text
Port
↓
PID
↓
Executable
↓
CWD
↓
Parent Process
↓
Project
↓
Git Repository
↓
Workspace
```

## 7.2 Port Reservation

Agent 启动服务前：

```text
reserve_port(project, service)
```

返回：

```text
3000
```

如果被占用：

```text
preferred 3000 occupied
allocated 3003
```

## 7.3 冲突策略

支持：

```text
reuse
allocate-next
fail
ask
kill-existing
```

默认：

```text
Same Service → reuse
Same Project → warn
Different Project → allocate-next
Unknown Process → never kill automatically
```

这是安全上的重要默认值。

---

# 8. Process Lifecycle

Runtime Core 暴露：

```text
start_service
stop_service
restart_service
get_service
list_services
```

停止策略：

```text
SIGTERM / graceful
↓ timeout
force terminate
```

Windows 需要对应：

```text
CTRL_BREAK / terminate process tree
```

必须使用 Process Identity：

```text
PID + Process Start Time
```

避免 PID 被 OS 回收后误杀新的进程。

---

# 9. macOS Adapter

## 9.1 Process

候选方式：

- sysctl
- libproc
- `lsof` 作为 fallback
- Rust `sysinfo` 作为通用层

## 9.2 Port

优先实现统一 abstraction。

必要时：

```text
lsof
netstat
libproc
```

## 9.3 Side Panel

macOS 原生能力：

```text
NSPanel
NSWindow
NSScreen
NSStatusItem
```

Tauri 默认 Window 无法覆盖所有 Sidebar 行为时：

```text
Rust
 ↓ FFI / Plugin
Swift
 ↓
AppKit
```

目标：

- Non-activating Panel
- Floating
- Screen-edge docking
- Auto hide
- Multi-screen
- Spaces compatibility

## 9.4 Menu Bar

使用：

```text
NSStatusItem
```

或 Tauri Tray API。

---

# 10. Windows Adapter

## 10.1 Process

可使用：

- Windows API
- Toolhelp32Snapshot
- WMI 作为补充
- Rust `sysinfo`

## 10.2 Port

候选：

```text
GetExtendedTcpTable
GetExtendedUdpTable
```

应避免依赖解析：

```text
netstat -ano
```

生产版本优先使用 Win32 API。

## 10.3 Sidebar

Windows 对应 macOS NSPanel 的能力建议通过：

```text
Win32
```

实现：

- Borderless Window
- Topmost
- Screen edge snapping
- Auto hide
- Multi monitor

可进一步研究：

```text
AppBar
SHAppBarMessage
```

是否采用 AppBar 取决于是否希望真正占据 Desktop Work Area。

推荐默认：

> Overlay Sidebar

而不是修改系统 Work Area。

## 10.4 System Tray

使用：

```text
Shell_NotifyIcon
```

或 Tauri Tray API。

---

# 11. OS Adapter Interface

核心业务禁止直接依赖 macOS / Windows API。

定义：

```text
trait ProcessProvider
trait PortProvider
trait WindowProvider
trait NotificationProvider
trait AutostartProvider
trait ShellProvider
```

结构：

```text
runtime-core
    │
    ├── adapter-macos
    │
    └── adapter-windows
```

这样未来增加 Linux：

```text
adapter-linux
```

不需要修改 Runtime Core。

---

# 12. MCP 设计

## 12.1 第一阶段 Tools

只暴露高价值动作。

### Runtime

```text
list_projects
get_project_runtime
list_services
get_service
```

### Lifecycle

```text
start_service
stop_service
restart_service
```

### Port

```text
check_port
reserve_port
release_port
resolve_port_conflict
```

### Logs

```text
get_logs
tail_logs
```

### Git

```text
list_worktrees
register_worktree
get_workspace
```

### Health

```text
get_health
wait_until_healthy
```

---

# 13. MCP 不应该提供什么

避免提供：

```text
execute_shell
kill_any_pid
run_arbitrary_command
```

MCP 应执行高层语义操作：

```text
restart_service("api")
```

而不是：

```text
exec("kill -9 8291")
```

这是安全边界的核心设计原则。

---

# 14. Claude Code 使用方式

安装：

```text
claude mcp add local-runtime ...
```

之后用户可以：

```text
Start this project's frontend and API.
```

Agent：

```text
get_project_runtime
↓
start_service(frontend)
start_service(api)
↓
wait_until_healthy
```

或者：

```text
Why is localhost:3000 unavailable?
```

Agent：

```text
check_port(3000)
↓
resolve owner
↓
return Project / Service / Agent
```

---

# 15. Agent Ownership

这是项目的重要差异点。

Runtime 记录：

```text
AgentSession
- id
- provider
- client
- cwd
- project
- startedAt
```

例如：

```text
provider = anthropic
client = claude-code
```

或：

```text
provider = openai
client = codex
```

Service Runtime：

```text
startedBy = claude-code
ownerSession = xxx
```

GUI 可以显示：

```text
● API :8000
  feature/refund
  Started by Claude Code
```

---

# 16. Git Worktree

必须在早期设计，而不是后期补。

例如：

```text
dossh
├── main
│   ├── web :3000
│   └── api :8000
│
└── feature/refund
    ├── web :3001
    └── api :8001
```

Port 分配策略：

```text
main
web = 3000
api = 8000

worktree #1
web = 3001
api = 8001

worktree #2
web = 3002
api = 8002
```

支持 Stable Worktree Port。

---

# 17. 项目自动识别

扫描：

```text
package.json
pnpm-workspace.yaml
turbo.json
vite.config.*
next.config.*
pyproject.toml
requirements.txt
Cargo.toml
docker-compose.yml
compose.yml
```

推断：

```text
Framework
Service Type
Default Command
Default Port
```

例如：

```text
Next.js
command: npm run dev
port: 3000
```

但自动推断结果必须允许用户确认和修改。

---

# 18. Configuration

每个项目允许：

```text
.runtime.json
```

例如：

```json
{
  "name": "dossh",
  "services": {
    "web": {
      "command": "pnpm dev",
      "port": 3000
    },
    "api": {
      "command": "pnpm api:dev",
      "port": 8000
    }
  }
}
```

后续可改名。

建议第一版不要设计复杂 DSL。

---

# 19. Logs

Runtime 启动 Process 时统一捕获：

```text
stdout
stderr
```

支持：

- Ring Buffer
- Persist optional
- Per Service logs
- Combined logs
- Search
- Filter
- Copy
- Follow

MCP 获取日志时需要：

```text
max_lines
since
service
```

防止 Agent 一次读取大量日志。

---

# 20. Health Check

支持：

```text
TCP
HTTP
Process
Custom
```

例如：

```text
HTTP GET /health
expected 200
```

Service 状态不要只有：

```text
running / stopped
```

而应区分：

```text
starting
healthy
unhealthy
stopping
stopped
failed
unknown
```

---

# 21. Docker

第一阶段不要求 Docker 管理，但 Core 应预留：

```text
RuntimeProvider
```

实现：

```text
NativeProcessProvider
DockerProvider
```

后续支持：

- docker compose service
- container logs
- container ports
- start/stop/restart

---

# 22. Security

## 22.1 最小权限

Daemon 不默认 root / Administrator。

只有确实需要时请求权限。

## 22.2 Kill Safety

禁止：

```text
kill_port(3000)
```

直接转换成无条件 Kill。

流程：

```text
Port
↓
Process Identity
↓
Ownership
↓
Project Match
↓
Permission Policy
↓
Terminate
```

## 22.3 Local API

若使用 localhost API：

- Random auth token
- Bind `127.0.0.1`
- 不监听 LAN
- Token 存 Keychain / Credential Manager

更推荐：

macOS：

```text
Unix Domain Socket
```

Windows：

```text
Named Pipe
```

作为最终 IPC。

---

# 23. 数据存储

推荐：

```text
SQLite
```

存：

- Projects
- Services
- Port Leases
- Workspaces
- Runtime metadata
- Settings

运行中的 Process 状态以 OS 为事实来源。

Daemon 启动后：

```text
DB state
+
OS process scan
↓
reconcile
```

避免数据库记录与实际进程长期不一致。

---

# 24. Repository 结构建议

```text
local-runtime/
│
├── apps/
│   └── desktop/
│
├── crates/
│   ├── runtime-core/
│   ├── runtime-daemon/
│   ├── adapter-macos/
│   ├── adapter-windows/
│   ├── runtime-cli/
│   └── runtime-ipc/
│
├── packages/
│   ├── runtime-mcp/
│   └── shared-types/
│
├── config/
│
└── docs/
```

---

# 25. 第一阶段 MVP

目标：

> 能真正替代开发者手动管理几个本地项目，而不是一次性 Demo。

## Phase 0 — Core PoC

实现：

- Process List
- TCP Port List
- PID → CWD
- PID → Project
- Start Process
- Stop Process
- stdout/stderr Capture

平台：

- macOS
- Windows

暂时 CLI 输出即可。

### 验收

同一套 Rust Core：

```text
runtime list
```

在两个平台均能输出：

```text
project
service
pid
port
cwd
```

---

# 26. Phase 1 — Runtime Daemon

实现：

- Daemon
- IPC
- SQLite
- Project Registry
- Service Registry
- Port Registry
- Lifecycle
- Reconciliation

CLI：

```text
runtime project list
runtime service list
runtime start
runtime stop
runtime restart
runtime port check
```

此阶段完成后，底层架构基本定型。

---

# 27. Phase 2 — Desktop MVP

Tauri Desktop。

实现：

- Main Window
- Project Sidebar
- Service List
- Port Status
- Start / Stop / Restart
- Open localhost
- Logs
- Tray

macOS / Windows 同功能。

暂时不要求 Edge Sidebar。

---

# 28. Phase 3 — MCP

实现：

```text
list_projects
get_project_runtime
start_service
stop_service
restart_service
check_port
get_logs
get_health
```

优先接入：

1. Claude Code
2. Codex
3. Cursor

MCP 不直接持有 Runtime 状态，只调用 Daemon。

---

# 29. Phase 4 — Native Edge Sidebar

macOS：

```text
NSPanel
```

Windows：

```text
Win32 Floating Window
```

实现：

- Edge docking
- Auto hide
- Always on top
- Compact mode
- Multi monitor

这一阶段才形成产品视觉差异。

---

# 30. Phase 5 — Project Intelligence

实现：

- Project Detection
- Framework Detection
- Default Command Detection
- Git Branch
- Worktree
- Stable Port Allocation

例如：

```text
Detected:
Next.js
pnpm
Git worktree
```

自动建议：

```text
web → pnpm dev → 3000
```

---

# 31. Phase 6 — Agent-Aware Runtime

实现：

- Agent Session
- startedBy
- ownership
- concurrent agents
- port reservation
- safe conflict resolution

最终 UI：

```text
DOSSH

● web :3000
  main
  Claude Code

● api :8000
  feature/refund
  Codex
```

---

# 32. Phase 7 — Enhanced Runtime

候选：

- Docker
- Tunnel
- Cloudflare Tunnel
- ngrok
- HTTPS local cert
- Reverse proxy
- `.localhost` domains
- Team project presets
- IDE plugins
- Runtime history
- CPU / Memory
- Crash detection
- Auto restart

---

# 33. MVP 范围控制

第一版明确不做：

- Remote Server Management
- Kubernetes
- Full Docker Desktop Replacement
- SSH Server Manager
- IDE
- Terminal Emulator
- CI/CD
- Production Process Manager
- Arbitrary shell Agent

防止项目失控。

---

# 34. 第一版用户故事

## User Story 1

开发者打开 App：

```text
DOSSH
● Web :3000
● API :8000

LOOM
● Web :3001
```

立即知道 localhost 属于哪个项目。

## User Story 2

用户点击：

```text
Restart API
```

无需寻找对应 Terminal。

## User Story 3

Claude Code：

```text
Restart the API and wait until it is healthy.
```

MCP 完成：

```text
restart
health check
```

## User Story 4

Claude Code 新建 Worktree。

Runtime：

```text
register worktree
reserve ports
```

避免和 main 冲突。

## User Story 5

Codex 准备启动 `:3000`。

Runtime 返回：

```text
3000 belongs to DOSSH/main/web
Suggested port: 3002
```

---

# 35. 最重要的技术验证

在投入完整 UI 前，优先验证四件事。

## A. macOS Process → CWD → Port

必须稳定。

## B. Windows Process → CWD → Port

必须稳定。

## C. Daemon + IPC

GUI / CLI / MCP 同时连接不会出现状态竞争。

## D. Process Tree Termination

Node / Python Dev Server 常有子进程。

必须确保：

```text
restart
```

不会留下 orphan processes。

如果这四项验证失败，GUI 做得再漂亮也没有意义。

---

# 36. 推荐开发顺序

```text
Cross-platform Process / Port PoC
            ↓
        Runtime Core
            ↓
          Daemon
            ↓
           CLI
            ↓
      Desktop Main UI
            ↓
           MCP
            ↓
      Native Sidebar
            ↓
        Worktree
            ↓
       Agent Ownership
```

不要先做 Sidebar。

Sidebar 是产品外观。

Runtime Core 才是产品壁垒。

---

# 37. 产品差异化优先级

从高到低：

### P0

- Project-aware
- Cross-platform
- Unified Runtime
- GUI + CLI + MCP

### P1

- Worktree-aware
- Stable Port Allocation
- Process Ownership
- Logs / Health

### P2

- Native Edge Sidebar
- Agent-aware
- Conflict Resolution

### P3

- Docker
- Tunnel
- Reverse Proxy
- Team Profiles

---

# 38. 推荐最终产品定义

不要把官网文案写成：

> A better port manager.

更准确的定位是：

> A local runtime manager for developers and coding agents.

或者：

> One place to see and control everything running on localhost.

产品模型：

```text
Human
  │
  ├ Desktop
  ├ Sidebar
  └ CLI
       │
       ▼
 Local Runtime
       ▲
       │
  ┌────┴─────┐
Claude      Codex
Code        Cursor
```

端口管理是底层能力。

真正的产品是：

> 开发者与 AI Agent 共享的本地运行时状态与控制层。
