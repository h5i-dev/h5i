use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use git2::Repository;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ctx;
use crate::error::H5iError;

#[derive(Debug, Clone)]
pub struct CodexSyncResult {
    pub session_id: String,
    pub observed: usize,
    pub acted: usize,
    pub processed_lines: usize,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct CodexSyncState {
    session_id: String,
    processed_lines: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TraceEvent {
    kind: &'static str,
    message: String,
}

pub fn find_latest_session(workdir: &Path) -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let sessions_root = PathBuf::from(home).join(".codex").join("sessions");
    let mut candidates = Vec::new();
    collect_jsonl(&sessions_root, &mut candidates);
    let mut matches: Vec<(std::time::SystemTime, PathBuf)> = candidates
        .into_iter()
        .filter_map(|path| {
            let modified = path.metadata().ok()?.modified().ok()?;
            if session_cwd_matches(&path, workdir) {
                Some((modified, path))
            } else {
                None
            }
        })
        .collect();
    matches.sort_by(|a, b| b.0.cmp(&a.0));
    matches.into_iter().next().map(|(_, path)| path)
}

pub fn sync_context(workdir: &Path) -> Result<Option<CodexSyncResult>, H5iError> {
    let Some(session_path) = find_latest_session(workdir) else {
        return Ok(None);
    };

    let raw = fs::read_to_string(&session_path)?;
    let session_id = session_path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let total_lines = raw.lines().count();

    let state_path = sync_state_path(workdir)?;
    let state = read_sync_state(&state_path);
    let start_line = if state.session_id == session_id {
        state.processed_lines.min(total_lines)
    } else {
        0
    };

    let mut observed = 0usize;
    let mut acted = 0usize;

    for line in raw.lines().skip(start_line) {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        for event in extract_events(&value, workdir) {
            ctx::append_log(workdir, event.kind, &event.message, false)?;
            match event.kind {
                "OBSERVE" => observed += 1,
                "ACT" => acted += 1,
                _ => {}
            }
        }
    }

    let next_state = CodexSyncState {
        session_id: session_id.clone(),
        processed_lines: total_lines,
    };
    fs::write(&state_path, serde_json::to_string_pretty(&next_state)?)?;

    Ok(Some(CodexSyncResult {
        session_id,
        observed,
        acted,
        processed_lines: total_lines.saturating_sub(start_line),
    }))
}

fn sync_state_path(workdir: &Path) -> Result<PathBuf, H5iError> {
    let repo = Repository::discover(workdir)?;
    let h5i_root = repo.path().join(".h5i");
    fs::create_dir_all(&h5i_root)?;
    Ok(h5i_root.join("codex_sync_state.json"))
}

fn read_sync_state(path: &Path) -> CodexSyncState {
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<CodexSyncState>(&raw).ok())
        .unwrap_or_default()
}

fn collect_jsonl(dir: &Path, out: &mut Vec<PathBuf>) {
    collect_jsonl_depth(dir, out, 0);
}

fn collect_jsonl_depth(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 4 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_jsonl_depth(&path, out, depth + 1);
        } else if path.extension().and_then(|s| s.to_str()) == Some("jsonl") {
            out.push(path);
        }
    }
}

fn session_cwd_matches(session_path: &Path, workdir: &Path) -> bool {
    let target = normalize_display_path(workdir, workdir);
    let Ok(file) = fs::File::open(session_path) else {
        return false;
    };
    BufReader::new(file).lines().take(40).any(|line| {
        let Ok(line) = line else { return false };
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            return false;
        };
        let cwd = value
            .pointer("/payload/cwd")
            .and_then(Value::as_str)
            .or_else(|| value.pointer("/payload/metadata/cwd").and_then(Value::as_str));
        match cwd {
            Some(cwd) => normalize_display_path(workdir, Path::new(cwd)) == target,
            None => false,
        }
    })
}

fn extract_events(value: &Value, workdir: &Path) -> Vec<TraceEvent> {
    let item_type = value.get("type").and_then(Value::as_str).unwrap_or_default();
    match item_type {
        // Older Codex format: structured exec_command_end with parsed_cmd array.
        "event_msg" => extract_exec_command_events(value, workdir),
        // Current Codex: function_call items — handles exec_command (OBSERVE) and
        // legacy apply_patch (ACT) for older session formats.
        "response_item" => extract_response_item_events(value, workdir),
        // Current Codex: apply_patch is emitted as custom_tool_call, not function_call.
        "custom_tool_call" => extract_custom_tool_call_events(value),
        _ => Vec::new(),
    }
}

fn extract_exec_command_events(value: &Value, workdir: &Path) -> Vec<TraceEvent> {
    if value.pointer("/payload/type").and_then(Value::as_str) != Some("exec_command_end") {
        return Vec::new();
    }
    let mut events = Vec::new();
    let parsed = value
        .pointer("/payload/parsed_cmd")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for cmd in parsed {
        let cmd_type = cmd.get("type").and_then(Value::as_str).unwrap_or_default();
        match cmd_type {
            "read" => {
                if let Some(path) = cmd
                    .get("path")
                    .and_then(Value::as_str)
                    .map(|path| render_path(workdir, path))
                {
                    events.push(TraceEvent {
                        kind: "OBSERVE",
                        message: format!("read {path}"),
                    });
                }
            }
            "search" => {
                let query = cmd.get("query").and_then(Value::as_str).unwrap_or_default();
                let path = cmd
                    .get("path")
                    .and_then(Value::as_str)
                    .map(|path| render_path(workdir, path))
                    .unwrap_or_else(|| ".".to_string());
                let message = if query.is_empty() {
                    format!("searched {path}")
                } else {
                    format!("searched {path} for \"{query}\"")
                };
                events.push(TraceEvent {
                    kind: "OBSERVE",
                    message,
                });
            }
            "list_files" => {
                let path = cmd
                    .get("path")
                    .and_then(Value::as_str)
                    .map(|path| render_path(workdir, path))
                    .unwrap_or_else(|| ".".to_string());
                events.push(TraceEvent {
                    kind: "OBSERVE",
                    message: format!("listed files under {path}"),
                });
            }
            _ => {}
        }
    }
    events
}

fn extract_response_item_events(value: &Value, workdir: &Path) -> Vec<TraceEvent> {
    let payload_type = value
        .pointer("/payload/type")
        .and_then(Value::as_str)
        .unwrap_or_default();

    match payload_type {
        "function_call" => {
            match value.pointer("/payload/name").and_then(Value::as_str) {
                // Legacy path: apply_patch as a plain function_call (older Codex versions).
                Some("apply_patch") => value
                    .pointer("/payload/arguments")
                    .and_then(Value::as_str)
                    .map(extract_patch_events)
                    .unwrap_or_default(),
                // Current Codex: shell commands are function_call items with name "exec_command"
                // and an `arguments` JSON string containing a "cmd" key.
                Some("exec_command") => value
                    .pointer("/payload/arguments")
                    .and_then(Value::as_str)
                    .and_then(|args| serde_json::from_str::<Value>(args).ok())
                    .and_then(|obj| {
                        obj.get("cmd")
                            .and_then(Value::as_str)
                            .map(|cmd| extract_shell_cmd_events(cmd, workdir))
                    })
                    .unwrap_or_default(),
                _ => Vec::new(),
            }
        }
        // Current Codex: apply_patch is wrapped in a response_item whose payload has
        // type "custom_tool_call".  The patch text is in payload.input (not arguments).
        "custom_tool_call" => {
            if value.pointer("/payload/name").and_then(Value::as_str) != Some("apply_patch") {
                return Vec::new();
            }
            value
                .pointer("/payload/input")
                .and_then(Value::as_str)
                .map(extract_patch_events)
                .unwrap_or_default()
        }
        _ => Vec::new(),
    }
}

// Top-level custom_tool_call (kept for forward-compatibility with potential
// Codex format variations where apply_patch is not wrapped in response_item).
fn extract_custom_tool_call_events(value: &Value) -> Vec<TraceEvent> {
    if value.get("name").and_then(Value::as_str) != Some("apply_patch") {
        return Vec::new();
    }
    value
        .get("input")
        .and_then(Value::as_str)
        .map(extract_patch_events)
        .unwrap_or_default()
}

// Heuristic OBSERVE extraction from a raw shell command string.
// Handles the most common Codex read/search patterns; ignores build/compile/test
// commands that are not explorations of the codebase.
fn extract_shell_cmd_events(cmd: &str, workdir: &Path) -> Vec<TraceEvent> {
    // Only examine the first command in a pipeline or sequence.
    let cmd = cmd
        .split_once(" | ")
        .or_else(|| cmd.split_once(" && "))
        .or_else(|| cmd.split_once(" ; "))
        .map_or(cmd, |(left, _)| left);

    let tokens: Vec<&str> = cmd.split_whitespace().collect();
    let Some(&bin_path) = tokens.first() else {
        return Vec::new();
    };
    let bin = bin_path.rsplit('/').next().unwrap_or(bin_path);

    // Non-flag, non-quoted arguments (skip -flag, --flag, 'expr', "expr").
    let plain_args: Vec<&str> = tokens[1..]
        .iter()
        .filter(|t| !t.starts_with('-') && !t.starts_with('\'') && !t.starts_with('"'))
        .copied()
        .collect();

    match bin {
        "cat" | "head" | "tail" | "bat" | "less" | "more" => plain_args
            .iter()
            .map(|p| TraceEvent {
                kind: "OBSERVE",
                message: format!("read {}", render_path(workdir, p)),
            })
            .collect(),

        // sed -n 'Np' file — last plain arg is the file; the expression is quoted.
        "sed" => plain_args
            .last()
            .map(|p| TraceEvent {
                kind: "OBSERVE",
                message: format!("read {}", render_path(workdir, p)),
            })
            .into_iter()
            .collect(),

        "grep" | "rg" | "ag" | "ack" => {
            // rg --files [dir] — list all tracked files.
            if tokens[1..].contains(&"--files") {
                let path = plain_args.first().copied().unwrap_or(".");
                return vec![TraceEvent {
                    kind: "OBSERVE",
                    message: format!("listed files under {}", render_path(workdir, path)),
                }];
            }
            match plain_args.as_slice() {
                [] => Vec::new(),
                [path] => vec![TraceEvent {
                    kind: "OBSERVE",
                    message: format!("searched {}", render_path(workdir, path)),
                }],
                [query, path, ..] => vec![TraceEvent {
                    kind: "OBSERVE",
                    message: format!(
                        "searched {} for \"{}\"",
                        render_path(workdir, path),
                        query
                    ),
                }],
            }
        }

        "find" | "fd" => {
            let path = plain_args.first().copied().unwrap_or(".");
            vec![TraceEvent {
                kind: "OBSERVE",
                message: format!("listed files under {}", render_path(workdir, path)),
            }]
        }

        "ls" => {
            let path = plain_args.first().copied().unwrap_or(".");
            vec![TraceEvent {
                kind: "OBSERVE",
                message: format!("listed files under {}", render_path(workdir, path)),
            }]
        }

        _ => Vec::new(),
    }
}

// Parses Codex's apply_patch dialect: lines beginning with "*** Update File: ",
// "*** Add File: ", or "*** Delete File: " declare file-level actions.
fn extract_patch_events(arguments: &str) -> Vec<TraceEvent> {
    let mut events = Vec::new();
    for line in arguments.lines() {
        if let Some(path) = line.strip_prefix("*** Update File: ") {
            events.push(TraceEvent {
                kind: "ACT",
                message: format!("edited {}", path.trim()),
            });
        } else if let Some(path) = line.strip_prefix("*** Add File: ") {
            events.push(TraceEvent {
                kind: "ACT",
                message: format!("added {}", path.trim()),
            });
        } else if let Some(path) = line.strip_prefix("*** Delete File: ") {
            events.push(TraceEvent {
                kind: "ACT",
                message: format!("deleted {}", path.trim()),
            });
        }
    }
    events
}

fn render_path(workdir: &Path, raw: &str) -> String {
    let candidate = Path::new(raw);
    if raw == "." {
        ".".to_string()
    } else if candidate.is_absolute() {
        normalize_display_path(workdir, candidate)
    } else {
        raw.trim_start_matches("./").to_string()
    }
}

fn normalize_display_path(workdir: &Path, path: &Path) -> String {
    path.strip_prefix(workdir)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::{extract_events, extract_patch_events, extract_shell_cmd_events, TraceEvent};
    use serde_json::json;
    use tempfile::tempdir;

    #[test]
    fn extract_patch_events_reports_file_actions() {
        let patch = "\
*** Begin Patch
*** Update File: src/main.rs
*** Add File: src/codex.rs
*** Delete File: old.txt
*** End Patch
";
        let events = extract_patch_events(patch);
        assert_eq!(
            events,
            vec![
                TraceEvent {
                    kind: "ACT",
                    message: "edited src/main.rs".into(),
                },
                TraceEvent {
                    kind: "ACT",
                    message: "added src/codex.rs".into(),
                },
                TraceEvent {
                    kind: "ACT",
                    message: "deleted old.txt".into(),
                },
            ]
        );
    }

    #[test]
    fn extract_events_reads_parsed_exec_commands() {
        let dir = tempdir().unwrap();
        let event = json!({
            "type": "event_msg",
            "payload": {
                "type": "exec_command_end",
                "parsed_cmd": [
                    { "type": "read", "path": dir.path().join("src/main.rs").display().to_string() },
                    { "type": "search", "path": ".", "query": "Codex" },
                    { "type": "list_files", "path": "." }
                ]
            }
        });

        let events = extract_events(&event, dir.path());
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].message, "read src/main.rs");
        assert_eq!(events[1].message, "searched . for \"Codex\"");
        assert_eq!(events[2].message, "listed files under .");
    }

    // ── custom_tool_call / apply_patch (current Codex format) ────────────────

    // apply_patch wrapped in response_item (actual current Codex format)
    #[test]
    fn extract_events_response_item_custom_tool_call_apply_patch() {
        let dir = tempdir().unwrap();
        let event = json!({
            "type": "response_item",
            "payload": {
                "type": "custom_tool_call",
                "status": "completed",
                "name": "apply_patch",
                "input": "*** Begin Patch\n*** Update File: main.py\n*** Add File: helper.py\n*** End Patch\n"
            }
        });
        let events = extract_events(&event, dir.path());
        assert_eq!(events.len(), 2);
        assert_eq!(events[0], TraceEvent { kind: "ACT", message: "edited main.py".into() });
        assert_eq!(events[1], TraceEvent { kind: "ACT", message: "added helper.py".into() });
    }

    // top-level custom_tool_call (forward-compat / older Codex format)
    #[test]
    fn extract_events_top_level_custom_tool_call_apply_patch() {
        let dir = tempdir().unwrap();
        let event = json!({
            "type": "custom_tool_call",
            "status": "completed",
            "name": "apply_patch",
            "input": "*** Begin Patch\n*** Update File: src/lib.rs\n*** End Patch\n"
        });
        let events = extract_events(&event, dir.path());
        assert_eq!(events.len(), 1);
        assert_eq!(events[0], TraceEvent { kind: "ACT", message: "edited src/lib.rs".into() });
    }

    #[test]
    fn extract_events_custom_tool_call_ignores_other_names() {
        let dir = tempdir().unwrap();
        let event = json!({
            "type": "response_item",
            "payload": { "type": "custom_tool_call", "name": "exec_command", "input": "x" }
        });
        assert!(extract_events(&event, dir.path()).is_empty());
    }

    // ── response_item / exec_command (current Codex format) ──────────────────

    #[test]
    fn extract_events_exec_command_sed_read() {
        let dir = tempdir().unwrap();
        let event = json!({
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "name": "exec_command",
                "arguments": serde_json::to_string(&json!({
                    "cmd": format!("sed -n '1,220p' {}/main.py", dir.path().display()),
                    "workdir": dir.path().display().to_string()
                })).unwrap()
            }
        });
        let events = extract_events(&event, dir.path());
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "OBSERVE");
        assert_eq!(events[0].message, "read main.py");
    }

    #[test]
    fn extract_events_exec_command_rg_files() {
        let dir = tempdir().unwrap();
        let event = json!({
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "name": "exec_command",
                "arguments": serde_json::to_string(&json!({
                    "cmd": "rg --files",
                    "workdir": dir.path().display().to_string()
                })).unwrap()
            }
        });
        let events = extract_events(&event, dir.path());
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "OBSERVE");
        assert!(events[0].message.starts_with("listed files"));
    }

    // ── extract_shell_cmd_events unit tests ───────────────────────────────────

    #[test]
    fn shell_cmd_sed_extracts_filename() {
        let dir = tempdir().unwrap();
        let events = extract_shell_cmd_events("sed -n '1,50p' main.py", dir.path());
        assert_eq!(events, vec![TraceEvent { kind: "OBSERVE", message: "read main.py".into() }]);
    }

    #[test]
    fn shell_cmd_cat_multiple_files() {
        let dir = tempdir().unwrap();
        let events = extract_shell_cmd_events("cat foo.py bar.py", dir.path());
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].message, "read foo.py");
        assert_eq!(events[1].message, "read bar.py");
    }

    #[test]
    fn shell_cmd_rg_files_flag() {
        let dir = tempdir().unwrap();
        let events = extract_shell_cmd_events("rg --files", dir.path());
        assert_eq!(events, vec![TraceEvent { kind: "OBSERVE", message: "listed files under .".into() }]);
    }

    #[test]
    fn shell_cmd_rg_search_with_path() {
        let dir = tempdir().unwrap();
        let events = extract_shell_cmd_events("rg fetch_user src/", dir.path());
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].message, "searched src/ for \"fetch_user\"");
    }

    #[test]
    fn shell_cmd_compile_ignored() {
        let dir = tempdir().unwrap();
        assert!(extract_shell_cmd_events("python3 -m py_compile main.py", dir.path()).is_empty());
        assert!(extract_shell_cmd_events("cargo build", dir.path()).is_empty());
    }

    #[test]
    fn shell_cmd_pipeline_only_first_command() {
        let dir = tempdir().unwrap();
        // Only the `sed` part should be parsed; `grep` in the pipe should be ignored.
        let events = extract_shell_cmd_events("sed -n '1,10p' main.py | grep def", dir.path());
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].message, "read main.py");
    }
}
