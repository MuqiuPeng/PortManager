# Windows adapter — porting guide

This is the handoff document for the Windows half of the runtime. Everything in
`crates/adapter-windows` is yours; nothing outside it should need to change.

## What already works

`WindowsAdapter` is a **working baseline**, not a stub. Building the workspace
on Windows today gives you a functioning `runtime` and `runtime-daemon`:

| Concern | Baseline | Where |
|---|---|---|
| Process list, cwd, argv | `sysinfo` | `runtime-adapter/src/generic.rs` |
| Listening ports -> pid | `netstat2` (`GetExtendedTcpTable` under the hood) | `runtime-adapter/src/generic.rs` |
| New process group | `CREATE_NEW_PROCESS_GROUP \| CREATE_NO_WINDOW` | `adapter-windows/src/spawn.rs` |
| Tree termination | `taskkill /T /F` | `adapter-windows/src/process.rs` |
| IPC | named pipe `\\.\pipe\local-runtime` | `runtime-ipc/src/transport.rs` |

Start here:

```powershell
cargo build --workspace
.\target\debug\runtime.exe doctor
```

`runtime doctor` is the acceptance test for the adapter. It reports how many
processes are visible, **how many of those have a working directory**, how many
ports are listening, and whether the adapter can see its own process. If the
cwd count is zero, port-to-project resolution cannot work and nothing else
matters — that is the first thing to fix.

> On macOS this check caught exactly that: `sysinfo` returns `None` for cwd, and
> the adapter had to call `proc_pidinfo(PROC_PIDVNODEPATHINFO)` natively.
> Verify the equivalent on Windows before trusting the baseline.

## The work, in priority order

### 1. Graceful termination (highest value)

The baseline always force-kills. `TerminationMode::Graceful` should first send
`GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid)` and only escalate after the
caller's grace period. The process group created in `WindowsSpawnProvider`
exists precisely so this event reaches the service and nothing else.

`runtime-core` already implements the escalation timer: it calls
`terminate_tree(Graceful)`, polls `is_alive`, and calls `terminate_tree(Forceful)`
when the deadline passes. You only need the two modes to actually differ.

**Why it matters:** a force-killed dev server does not flush its state, and
`restart` becomes lossy. See `crates/runtime-core/src/lifecycle.rs`.

### 2. Job Objects for tree termination

Replace `taskkill` with a Job Object created in `prepare()` and assigned to the
child, with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. Closing the handle then
guarantees no descendant survives, even one that re-parents itself — which
`taskkill /T` does not, because it walks a parent chain that a detached
grandchild has already left.

This requires storing the job handle alongside the child. `Supervisor`
(`runtime-core/src/supervisor.rs`) already owns a per-service record; if the
handle needs to live there, add it behind a `#[cfg(windows)]` field rather than
changing the trait.

**Acceptance:** start a service whose command spawns a detached grandchild, stop
it, and confirm the grandchild is gone. `demo/server.js` in the repo history
does exactly this and is what the macOS side was validated against.

### 3. Native port table

Swap `GenericPortProvider` for `GetExtendedTcpTable` / `GetExtendedUdpTable`
directly. `netstat2` already uses these, so this is a performance and
dependency-surface change, not a correctness one — do it after 1 and 2.

Do **not** parse `netstat -ano`. The plan is explicit about the API path for the
shipped version, and the output format is localised.

### 4. Native process metadata

`Toolhelp32Snapshot` + `QueryFullProcessImageName` avoids the full-system
refresh `sysinfo` performs on every call. Only worth doing if `doctor` shows the
process walk is slow, or if `sysinfo` turns out not to report cwd.

### 5. The edge panel

`crates/runtime-adapter/src/desktop.rs` declares `WindowProvider`; macOS
implements it in `adapter-macos/src/panel.rs`. The Windows side is not written.

What the trait needs:

- `adopt_panel` — the equivalent of macOS's non-activating `NSPanel`. On
  Windows that is `WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW` (plus `WS_EX_TOPMOST`),
  set with `SetWindowLongPtr(GWL_EXSTYLE)` on the HWND Tauri hands you.
  `WS_EX_NOACTIVATE` is the whole point: clicking the panel must not take focus
  from the editor.
- `show_panel` / `hide_panel` — `SetWindowPos` with `SWP_NOACTIVATE` for the
  passive case, `SetForegroundWindow` for the focused one.
- `screens` / `screen_at_pointer` — `EnumDisplayMonitors` + `GetMonitorInfo`
  for the work area (the equivalent of `visibleFrame`, excluding the taskbar),
  and `GetCursorPos`.

Two things to carry over from the macOS implementation:

**Verify the style actually applied.** `adopt_panel` on macOS reads the style
mask back and returns an error naming the window class if the non-activating
bit did not stick, because a silently failed adoption looks fine at startup and
only shows up later as a panel that steals focus. Do the same with
`GetWindowLongPtr`.

**Coordinates differ.** Cocoa's origin is bottom-left and Y grows upward;
Win32's is top-left. `frame_for` in `panel.rs` centres the panel from the bottom
for that reason — the Windows version centres from the top.

Do **not** use `SHAppBarMessage` to reserve desktop work area. The plan calls
for an overlay, and an AppBar would push every other window aside for something
that is on screen for a few seconds at a time. macOS has no equivalent, so the
two platforms would also diverge.

### 6. PowerShell shell selection

`shell()` returns `cmd.exe /C`, which is the safe default: it always exists and
resolves the `.cmd` shims npm and pnpm install. Users living in PowerShell
profiles will want their own environment; make it configurable rather than
switching the default.

## Rules the adapter must not break

These are enforced by the core and asserted by tests — they are the reason the
tool is safe to hand to an agent.

1. **Never terminate by pid alone.** `terminate_tree` receives a
   `ProcessIdentity` (`pid` + `process_start_time`) and must re-verify it
   immediately before signalling. Windows recycles pids aggressively; a stale
   record reaching `taskkill` would kill an unrelated process.
2. **Never terminate a process the runtime did not start.** The core enforces
   this before calling you (`PortResolver::reserve`), but do not add a code path
   that bypasses it.
3. **`terminate_tree` returns `Ok(false)`, not an error, when the process is
   already gone.** Callers treat that as success.
4. **Start times may be coarse.** `ProcessIdentity::matches` compares with a
   1.5s tolerance. If you can report a more precise start time
   (`GetProcessTimes` gives 100ns units), do — but do not assume the caller
   compares exactly.

## Testing your changes

```powershell
cargo test --workspace
cargo clippy --workspace --all-targets
```

From macOS or Linux the adapter can be type-checked without a Windows machine:

```bash
rustup target add x86_64-pc-windows-msvc
cargo check -p adapter-windows --target x86_64-pc-windows-msvc
```

Only the adapter crate: the full workspace cannot be cross-built from macOS
because `rusqlite`'s bundled SQLite needs a C toolchain for the target. That is
enough to keep the Windows crate from being left in a non-compiling state on the
shared branch.

## End-to-end acceptance

The same script should produce the same result on both platforms:

```powershell
$env:LOCAL_RUNTIME_DATA_DIR = "$env:TEMP\runtime-scratch"
runtime doctor                      # cwd count > 0, self lookup ok
runtime project add .               # services inferred
runtime start web --wait            # reports healthy on some port
runtime port check <that port>      # resolves to project/branch/service
runtime logs web                    # stdout captured
runtime stop web                    # no orphaned processes remain
runtime port check <that port>      # free again
```

If `runtime port check` says "unregistered process" for a service the runtime
itself started, the ancestor walk in `PortResolver::owner_of` is not finding the
process that actually bound the socket — check that `parent_pid` is populated.
