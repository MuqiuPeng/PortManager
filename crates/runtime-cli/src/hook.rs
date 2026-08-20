//! The Claude Code integration.
//!
//! A `PreToolUse` hook that records and returns. It does not rewrite the
//! command, and that is the design rather than a first step towards it:
//!
//! Rewriting `pnpm dev` into `runtime run -- pnpm dev` would put the runtime
//! between the agent and its own shell. The command shown for approval would no
//! longer be the command that runs, every allow-list the user wrote would stop
//! matching, and a daemon that is down or slow would break every shell command
//! on the machine. What that buys is ownership of the process tree at the
//! instant of birth — worth having, but not worth those costs, because it is
//! not what was actually missing.
//!
//! What was missing is knowledge. A running process will tell you its pid, its
//! port and its working directory, but not the command that would start it
//! again; that has to be inferred, and inferring it is how a project gets a
//! `dev` build written over the `start` build it needed. The hook supplies the
//! one fact that cannot be recovered later, and leaves everything else alone.
//!
//! Every failure path here exits 0 with no output. Nothing this program can go
//! wrong at is worth stopping a developer's command over.

use std::io::Read;
use std::path::{Path, PathBuf};

use runtime_ipc::client::Client;
use runtime_ipc::protocol::Request;
use serde::Deserialize;

/// The slice of Claude Code's hook payload this needs.
#[derive(Debug, Deserialize)]
struct HookInput {
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    cwd: Option<PathBuf>,
    #[serde(default)]
    tool_name: Option<String>,
    #[serde(default)]
    tool_input: Option<ToolInput>,
}

#[derive(Debug, Deserialize)]
struct ToolInput {
    #[serde(default)]
    command: Option<String>,
}

/// Read one hook payload and tell the daemon about it.
///
/// Returns nothing on stdout, which Claude Code reads as "proceed unchanged".
pub async fn pre_tool_use() {
    let mut raw = String::new();
    if std::io::stdin().read_to_string(&mut raw).is_err() {
        return;
    }
    let Ok(input) = serde_json::from_str::<HookInput>(&raw) else {
        return;
    };

    // Bash is the only tool that can start a server. A matcher in the settings
    // file should have taken care of this already; checking again costs
    // nothing and keeps the hook correct if it is installed more widely.
    if input.tool_name.as_deref() != Some("Bash") {
        return;
    }
    let (Some(command), Some(cwd)) = (
        input.tool_input.and_then(|tool| tool.command),
        input.cwd,
    ) else {
        return;
    };

    // Not connect_or_start: a shell command is not a reason to launch a daemon
    // the user has not started, and waiting for one to boot would put seconds
    // in front of every command.
    let Ok(mut client) = Client::connect_default().await else {
        return;
    };
    let _ = client
        .call(Request::RecordLaunch {
            command,
            cwd,
            source: Some("claude-code".to_string()),
            session: input.session_id,
        })
        .await;
}

// ---- installation ------------------------------------------------------

/// Where Claude Code reads hooks that apply to every project.
fn settings_path() -> Option<PathBuf> {
    Some(dirs_home()?.join(".claude").join("settings.json"))
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn executable() -> String {
    std::env::current_exe()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| "runtime".to_string())
}

/// Add the hook to the user's Claude Code settings.
///
/// Edits rather than writes: the file belongs to the user and usually has
/// permissions and other hooks in it that this has no business discarding.
pub fn install() -> Result<String, String> {
    let path = settings_path().ok_or("cannot find a home directory")?;
    let mut settings = read_settings(&path)?;

    let entry = serde_json::json!({
        "matcher": "Bash",
        "hooks": [{
            "type": "command",
            "command": format!("{} hook pre-tool-use", executable()),
            // Short: this runs in front of every shell command, and a runtime
            // that is wedged must not become a pause the user has to sit
            // through. Claude Code proceeds when a hook times out.
            "timeout": 5
        }]
    });

    let hooks = settings
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}));
    let hooks = hooks
        .as_object_mut()
        .ok_or("'hooks' in settings.json is not an object")?;
    let list = hooks
        .entry("PreToolUse")
        .or_insert_with(|| serde_json::json!([]));
    let list = list
        .as_array_mut()
        .ok_or("'hooks.PreToolUse' in settings.json is not a list")?;

    if let Some(existing) = list.iter_mut().find(|item| is_ours(item)) {
        *existing = entry;
        write_settings(&path, &settings)?;
        return Ok(format!("updated the hook in {}", path.display()));
    }

    list.push(entry);
    write_settings(&path, &settings)?;
    Ok(format!(
        "installed the hook in {}\nnew Claude Code sessions will record launches; existing ones keep the hooks they started with",
        path.display()
    ))
}

pub fn uninstall() -> Result<String, String> {
    let path = settings_path().ok_or("cannot find a home directory")?;
    let mut settings = read_settings(&path)?;

    let removed = settings
        .get_mut("hooks")
        .and_then(|hooks| hooks.get_mut("PreToolUse"))
        .and_then(|list| list.as_array_mut())
        .map(|list| {
            let before = list.len();
            list.retain(|item| !is_ours(item));
            before - list.len()
        })
        .unwrap_or(0);

    if removed == 0 {
        return Ok("the hook was not installed".to_string());
    }
    write_settings(&path, &settings)?;
    Ok(format!("removed the hook from {}", path.display()))
}

pub fn status() -> String {
    let Some(path) = settings_path() else {
        return "cannot find a home directory".to_string();
    };
    let installed = read_settings(&path)
        .map(|settings| {
            settings
                .get("hooks")
                .and_then(|hooks| hooks.get("PreToolUse"))
                .and_then(|list| list.as_array())
                .is_some_and(|list| list.iter().any(is_ours))
        })
        .unwrap_or(false);

    if installed {
        format!("installed in {}", path.display())
    } else {
        format!("not installed ({} has no entry)", path.display())
    }
}

/// Whether a `PreToolUse` entry is this program's.
///
/// Matched on the subcommand, not the absolute path: the binary moves between
/// a debug build, a release build and the bundle inside the app, and all three
/// are the same hook as far as the user is concerned.
fn is_ours(entry: &serde_json::Value) -> bool {
    entry
        .get("hooks")
        .and_then(|hooks| hooks.as_array())
        .is_some_and(|hooks| {
            hooks.iter().any(|hook| {
                hook.get("command")
                    .and_then(|command| command.as_str())
                    .is_some_and(|command| command.contains("hook pre-tool-use"))
            })
        })
}

fn read_settings(path: &Path) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    match std::fs::read_to_string(path) {
        Ok(raw) if raw.trim().is_empty() => Ok(serde_json::Map::new()),
        Ok(raw) => serde_json::from_str(&raw)
            .map_err(|err| format!("{} is not valid JSON: {err}", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(serde_json::Map::new()),
        Err(err) => Err(format!("{}: {err}", path.display())),
    }
}

fn write_settings(
    path: &Path,
    settings: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| format!("{}: {err}", parent.display()))?;
    }
    let body = serde_json::to_string_pretty(settings)
        .map_err(|err| format!("could not serialise settings: {err}"))?;
    std::fs::write(path, format!("{body}\n")).map_err(|err| format!("{}: {err}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entry_is_recognised_by_its_subcommand_not_its_path() {
        let ours = serde_json::json!({
            "matcher": "Bash",
            "hooks": [{"type": "command", "command": "/anywhere/at/all/runtime hook pre-tool-use"}]
        });
        assert!(is_ours(&ours));

        let theirs = serde_json::json!({
            "matcher": "Bash",
            "hooks": [{"type": "command", "command": "./scripts/audit.sh"}]
        });
        assert!(!is_ours(&theirs));
    }

    #[test]
    fn a_payload_for_another_tool_carries_no_command() {
        let input: HookInput = serde_json::from_str(
            r#"{"session_id":"s","cwd":"/repo","tool_name":"Read","tool_input":{"file_path":"/x"}}"#,
        )
        .unwrap();
        assert_eq!(input.tool_name.as_deref(), Some("Read"));
        assert!(input.tool_input.unwrap().command.is_none());
    }

    #[test]
    fn a_bash_payload_is_read_whole() {
        let input: HookInput = serde_json::from_str(
            r#"{"session_id":"s1","cwd":"/repo","hook_event_name":"PreToolUse",
                "tool_name":"Bash","tool_input":{"command":"cd web && pnpm dev","description":"x"}}"#,
        )
        .unwrap();
        assert_eq!(input.session_id.as_deref(), Some("s1"));
        assert_eq!(input.cwd, Some(PathBuf::from("/repo")));
        assert_eq!(
            input.tool_input.unwrap().command.as_deref(),
            Some("cd web && pnpm dev")
        );
    }

    #[test]
    fn unknown_fields_do_not_break_the_payload() {
        // Claude Code adds fields; a hook that fails to parse is a hook that
        // silently stops recording.
        let input: HookInput = serde_json::from_str(
            r#"{"tool_name":"Bash","tool_input":{"command":"pnpm dev"},"something_new":{"a":1}}"#,
        )
        .unwrap();
        assert_eq!(input.tool_name.as_deref(), Some("Bash"));
    }
}

// ---- the MCP server ----------------------------------------------------

/// Register the MCP server for every project this user works on.
///
/// User scope rather than per-project: the runtime is a property of the
/// machine, not of a checkout, and a tool that has to be added again in each
/// repository is one an agent will mostly not have. Stdio rather than a local
/// HTTP endpoint, because a port manager that needs a port of its own to be
/// reachable has an obvious failure mode on the day it is most needed.
pub fn install_mcp(command: &str) -> Result<String, String> {
    let output = std::process::Command::new("claude")
        .args([
            "mcp",
            "add",
            "--scope",
            "user",
            "--transport",
            "stdio",
            "localruntime",
            "--",
            command,
        ])
        .output()
        .map_err(|err| format!("could not run `claude`: {err}"))?;

    if output.status.success() {
        return Ok(format!(
            "registered '{command}' as the localruntime MCP server for every project"
        ));
    }
    let detail = String::from_utf8_lossy(&output.stderr);
    let detail = detail.trim();
    Err(if detail.is_empty() {
        "`claude mcp add` failed".to_string()
    } else {
        format!("`claude mcp add` failed: {detail}")
    })
}
