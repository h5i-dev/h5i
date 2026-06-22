//! Agent hook wiring — the pure config merges behind
//! `h5i hook setup --write`.

use crate::error::H5iError;
use serde_json::{Map, Value};
use toml::value::Table;

/// The hook entries `h5i hook setup --write` manages, as
/// `(event, matcher, command)`. Bash capture-wrapping is NOT here — it is
/// opt-in (`--wrap-bash`) because it changes what the agent sees for
/// large/failing commands (a `h5i capture run` summary instead of the raw
/// output).
const CORE_HOOKS: &[(&str, Option<&str>, &str)] = &[
    ("SessionStart", None, "h5i hook session-start"),
    ("PostToolUse", Some("Edit|Write|Read"), "h5i hook claude sync"),
    ("Stop", None, "h5i hook claude finish"),
    // Capture the verbatim human prompt so `h5i capture commit` records what
    // the human actually typed, not the agent's paraphrase.
    ("UserPromptSubmit", None, "h5i hook claude prompt"),
];

/// The opt-in Bash capture-wrap entry (PreToolUse: rewrites the command
/// into `h5i capture run` via updatedInput).
const WRAP_BASH_HOOK: (&str, Option<&str>, &str) =
    ("PreToolUse", Some("Bash"), "h5i hook wrap-bash");

const CODEX_STOP_HOOK: &str = "h5i hook codex finish --quiet";

/// The team peer-review Stop hook command, opt-in via `h5i hook setup --team`
/// (additive to the core set). It keeps an agent in a running team from
/// stopping while it still owes work and surfaces review requests between
/// turns. Both Claude Code and Codex Stop hooks honor the same
/// `{"decision":"block","reason":…}` continuation contract, so both wait-and-block
/// with `--block` (and both get the long per-hook timeout below).
const TEAM_HOOK_CLAUDE: (&str, Option<&str>, &str) =
    ("Stop", None, "h5i team agent hook --block");
const TEAM_HOOK_CODEX: (&str, Option<&str>, &str) = ("Stop", None, "h5i team agent hook --block");
/// Per-hook timeout (seconds) for the Claude team Stop hook. The hook blocks on
/// stop while it waits for the next review (default 30 min internal); Claude
/// Code would otherwise kill it at the ~60s default, so we pin a longer ceiling
/// with a small buffer above the hook's own wait.
const TEAM_HOOK_TIMEOUT_SECS: u64 = 1830;

/// Pre-rename command paths (`h5i claude …` / `h5i codex …`), superseded by the
/// `h5i hook …` forms. They still resolve via hidden CLI aliases, but the merge
/// strips any surviving managed entry so re-running `setup --write` migrates an
/// old install in place instead of leaving a duplicate that double-fires.
const LEGACY_AGENT_HOOKS: &[&str] = &[
    "h5i claude sync",
    "h5i claude finish",
    "h5i claude prompt",
    "h5i codex finish",
];

/// The retired PostToolUse Bash observation hook: superseded by wrap-bash
/// (which captures AND token-reduces). The subcommand no longer exists, so
/// a surviving entry would error on every Bash call — the merge always
/// strips it.
const LEGACY_OBSERVE_BASH: &str = "h5i hook observe-bash";

/// Idempotently merge the h5i hook wiring into a Claude Code `settings.json`
/// document: SessionStart (context prelude), PostToolUse on Edit|Write|Read
/// (auto-trace), Stop (auto-checkpoint), and — only when `wrap_bash` —
/// PreToolUse on Bash (`h5i hook wrap-bash`). Each managed command is
/// replaced in place if already present; everything else (env keys, the
/// `h5i msg hook` Stop entry, user hooks) is preserved, except the retired
/// `h5i hook observe-bash` entry which is always removed. Without
/// `wrap_bash` an *existing* wrap-bash entry is left alone — opting out of
/// adding it is not a request to remove it. `existing` may be empty
/// (treated as `{}`). Pure (no I/O) so it is unit-testable; the caller does
/// the file read/write.
pub fn merge_hook_settings_json(existing: &str, wrap_bash: bool) -> Result<String, H5iError> {
    let mut root: Value = if existing.trim().is_empty() {
        Value::Object(Map::new())
    } else {
        serde_json::from_str(existing)?
    };
    let obj = root
        .as_object_mut()
        .ok_or_else(|| H5iError::Metadata("settings.json is not a JSON object".into()))?;
    let hooks = obj
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()));
    let hooks_obj = hooks
        .as_object_mut()
        .ok_or_else(|| H5iError::Metadata("settings 'hooks' is not an object".into()))?;

    for &(event, matcher, command) in CORE_HOOKS {
        ensure_hook_entry(hooks_obj, event, matcher, command, None)?;
    }
    if let Some(arr) = hooks_obj
        .get_mut("PostToolUse")
        .and_then(|v| v.as_array_mut())
    {
        arr.retain(|entry| !entry_has_command(entry, LEGACY_OBSERVE_BASH));
    }
    // Drop any pre-rename `h5i claude …` / `h5i codex …` entries across every
    // event so a re-run migrates an old install rather than duplicating it.
    for arr in hooks_obj.values_mut().filter_map(|v| v.as_array_mut()) {
        arr.retain(|entry| {
            !LEGACY_AGENT_HOOKS
                .iter()
                .any(|legacy| entry_has_command(entry, legacy))
        });
    }
    if wrap_bash {
        let (event, matcher, command) = WRAP_BASH_HOOK;
        ensure_hook_entry(hooks_obj, event, matcher, command, None)?;
    }

    Ok(serde_json::to_string_pretty(&root)?)
}

/// The Linux path Claude Code reads as **managed settings** — the highest,
/// unoverridable scope. A hook here cannot be removed by higher-precedence
/// merge and survives a non-managed `disableAllHooks`. Injected at this exact
/// path inside a sandboxed box's private mount namespace (read-only) so the
/// in-box agent — which cannot write the root-owned `/etc` and cannot disable
/// a managed hook from its own writable config — has no way to silence
/// observation. Host-scoped to the box: the mount lives only in the box's ns.
pub const CLAUDE_MANAGED_SETTINGS_PATH: &str = "/etc/claude-code/managed-settings.json";

/// The minimal managed-settings.json injected into a sandboxed box: ONLY the
/// wrap-bash enforcement hook, not the core SessionStart/PostToolUse/Stop set.
/// The goal is to pin *observation* unkillably, not to override the agent's
/// own workflow hooks (which it remains free to configure in user/project
/// scope — managed hooks merge with, they don't replace, those). Pure.
pub fn managed_settings_wrap_bash_json() -> String {
    let mut hooks_obj = Map::new();
    let (event, matcher, command) = WRAP_BASH_HOOK;
    // ensure_hook_entry only errors if an existing entry has the wrong shape;
    // on a fresh map it cannot fail.
    let _ = ensure_hook_entry(&mut hooks_obj, event, matcher, command, None);
    let mut root = Map::new();
    root.insert("hooks".to_string(), Value::Object(hooks_obj));
    serde_json::to_string_pretty(&Value::Object(root)).unwrap_or_default()
}

/// Idempotently merge the h5i hook wiring into a Codex `config.toml` document.
/// Codex discovers inline `[hooks]` tables in `.codex/config.toml` or
/// `~/.codex/config.toml`; the shape is otherwise equivalent to Claude's
/// JSON hook arrays. User settings and unrelated hooks are preserved.
pub fn merge_codex_config_toml(existing: &str, wrap_bash: bool) -> Result<String, H5iError> {
    let mut root: toml::Value = if existing.trim().is_empty() {
        toml::Value::Table(Table::new())
    } else {
        toml::from_str(existing)?
    };
    let root_table = root
        .as_table_mut()
        .ok_or_else(|| H5iError::Metadata("config.toml is not a TOML table".into()))?;
    let hooks = root_table
        .entry("hooks".to_string())
        .or_insert_with(|| toml::Value::Table(Table::new()));
    let hooks_table = hooks
        .as_table_mut()
        .ok_or_else(|| H5iError::Metadata("config 'hooks' is not a table".into()))?;

    for &(event, matcher, command) in CORE_HOOKS {
        // UserPromptSubmit and PostToolUse are Claude-Code-specific here:
        // Codex prompt/tool provenance is mined from session JSONL by
        // `h5i hook codex finish`, installed below as the Stop hook.
        if matches!(event, "UserPromptSubmit" | "PostToolUse") {
            continue;
        }
        let command = match event {
            "Stop" => CODEX_STOP_HOOK,
            _ => command,
        };
        ensure_toml_hook_entry(hooks_table, event, matcher, command)?;
    }
    if let Some(arr) = hooks_table
        .get_mut("PostToolUse")
        .and_then(|v| v.as_array_mut())
    {
        arr.retain(|entry| !toml_entry_has_command(entry, LEGACY_OBSERVE_BASH));
    }
    // Drop any pre-rename `h5i codex …` entries across every event so a re-run
    // migrates an old install rather than duplicating it.
    for (_event, v) in hooks_table.iter_mut() {
        if let Some(arr) = v.as_array_mut() {
            arr.retain(|entry| {
                !LEGACY_AGENT_HOOKS
                    .iter()
                    .any(|legacy| toml_entry_has_command(entry, legacy))
            });
        }
    }
    if wrap_bash {
        let (event, matcher, command) = WRAP_BASH_HOOK;
        ensure_toml_hook_entry(hooks_table, event, matcher, command)?;
    }

    Ok(toml::to_string_pretty(&root)?)
}

/// Idempotently add the team peer-review Stop hook to a Claude `settings.json`
/// document, on top of whatever core hooks are already present. Additive: the
/// existing Stop entries (e.g. `h5i hook claude finish`) are preserved — a Stop
/// array may carry several entries that all fire. Pure; the caller does I/O.
pub fn merge_team_hook_settings_json(existing: &str) -> Result<String, H5iError> {
    let mut root: Value = if existing.trim().is_empty() {
        Value::Object(Map::new())
    } else {
        serde_json::from_str(existing)?
    };
    let obj = root
        .as_object_mut()
        .ok_or_else(|| H5iError::Metadata("settings.json is not a JSON object".into()))?;
    let hooks = obj
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()));
    let hooks_obj = hooks
        .as_object_mut()
        .ok_or_else(|| H5iError::Metadata("settings 'hooks' is not an object".into()))?;
    let (event, matcher, command) = TEAM_HOOK_CLAUDE;
    ensure_hook_entry(hooks_obj, event, matcher, command, Some(TEAM_HOOK_TIMEOUT_SECS))?;
    Ok(serde_json::to_string_pretty(&root)?)
}

/// Idempotently add the team peer-review Stop hook to a Codex `config.toml`
/// document. Codex's hook schema is `hooks.<event>` → an array of matcher
/// groups, each with a `hooks` array of `{type, command}` handlers. Rather than
/// push a *second* matcher-less `[[hooks.Stop]]` group beside the core
/// `h5i hook codex finish` one (valid, but Codex's docs only demonstrate one
/// group per event), append the team handler to the existing matcher-less Stop
/// group — and migrate any prior split layout by stripping the team command
/// from every group first. Mirrors [`merge_team_hook_settings_json`].
pub fn merge_team_hook_codex_toml(existing: &str) -> Result<String, H5iError> {
    let mut root: toml::Value = if existing.trim().is_empty() {
        toml::Value::Table(Table::new())
    } else {
        toml::from_str(existing)?
    };
    let root_table = root
        .as_table_mut()
        .ok_or_else(|| H5iError::Metadata("config.toml is not a TOML table".into()))?;
    let hooks_table = root_table
        .entry("hooks".to_string())
        .or_insert_with(|| toml::Value::Table(Table::new()))
        .as_table_mut()
        .ok_or_else(|| H5iError::Metadata("config 'hooks' is not a table".into()))?;
    let (event, _matcher, command) = TEAM_HOOK_CODEX;
    let arr = hooks_table
        .entry(event.to_string())
        .or_insert_with(|| toml::Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| H5iError::Metadata(format!("config hooks.{event} is not an array")))?;

    // Migrate/dedup: drop any prior team handler from every group (matching the
    // stable command prefix, so an arg change still migrates), then drop a group
    // left with no handlers.
    let is_team = |h: &toml::Value| {
        h.get("command")
            .and_then(|c| c.as_str())
            .map(|s| s.trim_start().starts_with("h5i team agent hook"))
            .unwrap_or(false)
    };
    for group in arr.iter_mut() {
        if let Some(hs) = group
            .as_table_mut()
            .and_then(|t| t.get_mut("hooks"))
            .and_then(|h| h.as_array_mut())
        {
            hs.retain(|h| !is_team(h));
        }
    }
    arr.retain(|g| {
        g.get("hooks")
            .and_then(|h| h.as_array())
            .map(|a| !a.is_empty())
            .unwrap_or(false)
    });

    let mut hook = Table::new();
    hook.insert("type".to_string(), toml::Value::String("command".to_string()));
    hook.insert("command".to_string(), toml::Value::String(command.to_string()));
    // The hook waits (up to ~30 min) for the next review; Codex would kill it at
    // its 600s default, so pin a longer per-handler timeout (it only matters for
    // this waiting handler, not the one-shot `codex finish`).
    hook.insert(
        "timeout".to_string(),
        toml::Value::Integer(TEAM_HOOK_TIMEOUT_SECS as i64),
    );

    // Append to the existing matcher-less Stop group (where `codex finish`
    // lives), else create one. Both handlers then fire from a single group.
    if let Some(group) = arr
        .iter_mut()
        .find(|g| g.as_table().map(|t| !t.contains_key("matcher")).unwrap_or(false))
    {
        group
            .as_table_mut()
            .unwrap()
            .entry("hooks".to_string())
            .or_insert_with(|| toml::Value::Array(Vec::new()))
            .as_array_mut()
            .ok_or_else(|| H5iError::Metadata("config hooks.Stop.hooks is not an array".into()))?
            .push(toml::Value::Table(hook));
    } else {
        let mut group = Table::new();
        group.insert(
            "hooks".to_string(),
            toml::Value::Array(vec![toml::Value::Table(hook)]),
        );
        arr.push(toml::Value::Table(group));
    }
    Ok(toml::to_string_pretty(&root)?)
}

/// Rewrite a Bash tool command into a token-reducing `h5i capture run`
/// invocation, or `None` when it must run untouched: h5i's own calls (a
/// wrapped `h5i recall object` would re-summarize bytes the agent explicitly
/// rehydrated, and `capture run`/`env run` already self-capture), commands
/// with a top-level `cd` (the harness tracks the session cwd from the outer
/// shell — a nested shell would swallow the change), and empty input.
///
/// A command made only of plain characters is passed straight as argv, so
/// `capture run`'s command-aware summary adapters (cargo/pytest/git) still
/// see the real argv[0]; anything with shell syntax (quotes, globs, pipes,
/// `$`, redirects, newlines …) runs via `bash -c '<single-quoted>'`, which
/// preserves its semantics exactly.
pub fn wrap_bash_command(command: &str) -> Option<String> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return None;
    }
    let first = trimmed.split_whitespace().next().unwrap_or("");
    let first_base = std::path::Path::new(first)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    if first_base == "h5i" {
        return None;
    }
    // `;`, `|`/`||`, `&`/`&&` and newlines all start a new top-level command.
    let has_top_level_cd = trimmed
        .split(['\n', ';', '|', '&'])
        .map(|seg| seg.trim_start().trim_start_matches(['(', '{']).trim_start())
        .any(|seg| seg == "cd" || seg.starts_with("cd ") || seg.starts_with("cd\t"));
    if has_top_level_cd {
        return None;
    }

    let simple = !trimmed
        .split_whitespace()
        .next()
        .unwrap_or("")
        .contains('=')
        && trimmed.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(
                    c,
                    ' ' | '_' | '-' | '.' | '/' | '=' | ':' | '@' | ',' | '+' | '%'
                )
        });
    if simple {
        Some(format!("h5i capture run -- {trimmed}"))
    } else {
        Some(format!(
            "h5i capture run -- bash -c {}",
            shell_single_quote(trimmed)
        ))
    }
}

/// POSIX single-quoting: wrap in `'…'`, encoding embedded `'` as `'\''`.
fn shell_single_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// Ensure `hooks.<event>` contains exactly one entry for `command`: drop any
/// prior entry carrying that command (so a re-run also refreshes the
/// matcher), then append `{ matcher?, hooks: [{type: command, command}] }`.
fn ensure_hook_entry(
    hooks_obj: &mut Map<String, Value>,
    event: &str,
    matcher: Option<&str>,
    command: &str,
    timeout: Option<u64>,
) -> Result<(), H5iError> {
    let arr = hooks_obj
        .entry(event)
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| H5iError::Metadata(format!("settings hooks.{event} is not an array")))?;
    arr.retain(|entry| !entry_has_command(entry, command));
    let mut entry = Map::new();
    if let Some(m) = matcher {
        entry.insert("matcher".to_string(), Value::String(m.to_string()));
    }
    let mut hook = Map::new();
    hook.insert("type".to_string(), Value::String("command".to_string()));
    hook.insert("command".to_string(), Value::String(command.to_string()));
    if let Some(t) = timeout {
        hook.insert("timeout".to_string(), Value::from(t));
    }
    entry.insert("hooks".to_string(), Value::Array(vec![Value::Object(hook)]));
    arr.push(Value::Object(entry));
    Ok(())
}

fn ensure_toml_hook_entry(
    hooks_table: &mut Table,
    event: &str,
    matcher: Option<&str>,
    command: &str,
) -> Result<(), H5iError> {
    let arr = hooks_table
        .entry(event.to_string())
        .or_insert_with(|| toml::Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| H5iError::Metadata(format!("config hooks.{event} is not an array")))?;
    arr.retain(|entry| !toml_entry_has_command(entry, command));

    let mut entry = Table::new();
    if let Some(m) = matcher {
        entry.insert("matcher".to_string(), toml::Value::String(m.to_string()));
    }
    let mut hook = Table::new();
    hook.insert(
        "type".to_string(),
        toml::Value::String("command".to_string()),
    );
    hook.insert(
        "command".to_string(),
        toml::Value::String(command.to_string()),
    );
    entry.insert(
        "hooks".to_string(),
        toml::Value::Array(vec![toml::Value::Table(hook)]),
    );
    arr.push(toml::Value::Table(entry));
    Ok(())
}

/// True if a hooks-array entry contains an inner command that is `command`
/// (exactly, or followed by arguments). Exact-or-space matching so
/// `h5i hook claude sync` never claims `h5i hook claude sync-something-else`.
fn entry_has_command(entry: &Value, command: &str) -> bool {
    entry
        .get("hooks")
        .and_then(|h| h.as_array())
        .map(|hs| {
            hs.iter().any(|hk| {
                hk.get("command")
                    .and_then(|c| c.as_str())
                    .map(|s| {
                        let s = s.trim_start();
                        s == command || s.strip_prefix(command).is_some_and(|r| r.starts_with(' '))
                    })
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

fn toml_entry_has_command(entry: &toml::Value, command: &str) -> bool {
    entry
        .get("hooks")
        .and_then(|h| h.as_array())
        .map(|hs| {
            hs.iter().any(|hk| {
                hk.get("command")
                    .and_then(|c| c.as_str())
                    .map(|s| {
                        let s = s.trim_start();
                        s == command || s.strip_prefix(command).is_some_and(|r| r.starts_with(' '))
                    })
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commands_under(root: &Value, event: &str) -> Vec<String> {
        root.pointer(&format!("/hooks/{event}"))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .flat_map(|e| {
                        e.get("hooks")
                            .and_then(|h| h.as_array())
                            .cloned()
                            .unwrap_or_default()
                    })
                    .filter_map(|hk| {
                        hk.get("command")
                            .and_then(|c| c.as_str())
                            .map(str::to_owned)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn fresh_default_has_core_hooks_but_no_wrap_bash() {
        let out = merge_hook_settings_json("", false).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            commands_under(&v, "SessionStart"),
            vec!["h5i hook session-start"]
        );
        assert_eq!(commands_under(&v, "PostToolUse"), vec!["h5i hook claude sync"]);
        assert_eq!(commands_under(&v, "Stop"), vec!["h5i hook claude finish"]);
        assert_eq!(
            commands_under(&v, "UserPromptSubmit"),
            vec!["h5i hook claude prompt"]
        );
        assert!(!out.contains("wrap-bash"));
        // The Edit|Write|Read matcher rides along with `h5i hook claude sync`.
        assert_eq!(
            v.pointer("/hooks/PostToolUse/0/matcher")
                .and_then(|m| m.as_str()),
            Some("Edit|Write|Read")
        );
    }

    #[test]
    fn wrap_bash_flag_adds_pretooluse_bash_entry() {
        let out = merge_hook_settings_json("{}", true).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let cmds = commands_under(&v, "PreToolUse");
        assert_eq!(cmds, vec!["h5i hook wrap-bash"]);
        let bash_entry = v
            .pointer("/hooks/PreToolUse")
            .and_then(|a| a.as_array())
            .unwrap()
            .iter()
            .find(|e| entry_has_command(e, "h5i hook wrap-bash"))
            .unwrap();
        assert_eq!(
            bash_entry.get("matcher").and_then(|m| m.as_str()),
            Some("Bash")
        );
    }

    #[test]
    fn team_flag_adds_team_stop_hook_claude() {
        let core = merge_hook_settings_json("", false).unwrap();
        let out = merge_team_hook_settings_json(&core).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let stop = commands_under(&v, "Stop");
        // Additive: the core finish hook stays; the team hook joins it.
        assert!(stop.contains(&"h5i hook claude finish".to_string()));
        assert!(stop.contains(&"h5i team agent hook --block".to_string()));
        // The team hook carries a long per-hook timeout so Claude Code doesn't
        // kill it while it waits for the next review.
        let team_hook = v
            .pointer("/hooks/Stop")
            .and_then(|s| s.as_array())
            .unwrap()
            .iter()
            .flat_map(|e| e.get("hooks").and_then(|h| h.as_array()).cloned().unwrap_or_default())
            .find(|h| {
                h.get("command").and_then(|c| c.as_str()) == Some("h5i team agent hook --block")
            })
            .unwrap();
        assert!(team_hook.get("timeout").and_then(|t| t.as_u64()).unwrap() >= 1800);
        // Idempotent: re-applying does not duplicate the entry.
        let twice = merge_team_hook_settings_json(&out).unwrap();
        let v2: Value = serde_json::from_str(&twice).unwrap();
        assert_eq!(
            commands_under(&v2, "Stop")
                .iter()
                .filter(|c| c.as_str() == "h5i team agent hook --block")
                .count(),
            1
        );
    }

    #[test]
    fn team_flag_adds_team_stop_hook_codex() {
        let core = merge_codex_config_toml("", false).unwrap();
        let out = merge_team_hook_codex_toml(&core).unwrap();
        // Codex blocks the stop just like Claude (same continuation contract).
        assert!(out.contains("h5i team agent hook --block"));
        // The core codex finish hook is preserved alongside it.
        assert!(out.contains("h5i hook codex finish --quiet"));

        // Consolidated into ONE Stop group with two handlers (the shape Codex's
        // docs demonstrate), not two separate `[[hooks.Stop]]` groups; the
        // waiting team handler carries a long per-handler timeout.
        let v: toml::Value = toml::from_str(&out).unwrap();
        let stop = v
            .get("hooks")
            .and_then(|h| h.get("Stop"))
            .and_then(|s| s.as_array())
            .unwrap();
        assert_eq!(stop.len(), 1, "exactly one Stop matcher group");
        let handlers = stop[0].get("hooks").and_then(|h| h.as_array()).unwrap();
        assert!(handlers.iter().any(|h| {
            h.get("command").and_then(|c| c.as_str()) == Some("h5i hook codex finish --quiet")
        }));
        let team = handlers
            .iter()
            .find(|h| {
                h.get("command").and_then(|c| c.as_str()) == Some("h5i team agent hook --block")
            })
            .unwrap();
        assert!(team.get("timeout").and_then(|t| t.as_integer()).unwrap() >= 1800);

        // Idempotent.
        let twice = merge_team_hook_codex_toml(&out).unwrap();
        assert_eq!(twice.matches("h5i team agent hook").count(), 1);

        // Migrates a prior split layout — even an old standalone `--quiet` group.
        let split = format!(
            "{out}\n[[hooks.Stop]]\n[[hooks.Stop.hooks]]\ntype = \"command\"\ncommand = \"h5i team agent hook --quiet\"\n"
        );
        let migrated = merge_team_hook_codex_toml(&split).unwrap();
        assert_eq!(migrated.matches("h5i team agent hook").count(), 1);
        assert!(migrated.contains("h5i team agent hook --block"));
        let mv: toml::Value = toml::from_str(&migrated).unwrap();
        assert_eq!(
            mv.get("hooks").and_then(|h| h.get("Stop")).and_then(|s| s.as_array()).unwrap().len(),
            1
        );
    }

    #[test]
    fn managed_settings_carries_only_wrap_bash() {
        let out = managed_settings_wrap_bash_json();
        let v: Value = serde_json::from_str(&out).unwrap();
        // Exactly the wrap-bash PreToolUse/Bash hook, and none of the core set
        // (managed scope pins observation, it does not commandeer the agent's
        // own SessionStart/PostToolUse/Stop wiring).
        assert_eq!(commands_under(&v, "PreToolUse"), vec!["h5i hook wrap-bash"]);
        for event in ["SessionStart", "PostToolUse", "Stop", "UserPromptSubmit"] {
            assert!(
                v.pointer(&format!("/hooks/{event}")).is_none(),
                "managed settings must not carry the {event} core hook"
            );
        }
        let entry = v
            .pointer("/hooks/PreToolUse")
            .and_then(|a| a.as_array())
            .unwrap()
            .iter()
            .find(|e| entry_has_command(e, "h5i hook wrap-bash"))
            .unwrap();
        assert_eq!(entry.get("matcher").and_then(|m| m.as_str()), Some("Bash"));
    }

    #[test]
    fn codex_config_toml_adds_core_hooks_and_preserves_settings() {
        let existing = r#"
model = "gpt-5.4"

[[hooks.Stop]]
[[hooks.Stop.hooks]]
type = "command"
command = "h5i msg hook"
"#;
        let out = merge_codex_config_toml(existing, false).unwrap();
        let v: toml::Value = toml::from_str(&out).unwrap();
        assert_eq!(v["model"].as_str(), Some("gpt-5.4"));
        assert!(out.contains("command = \"h5i hook session-start\""));
        assert!(!out.contains("command = \"h5i hook run\""));
        assert!(out.contains("command = \"h5i hook codex finish --quiet\""));
        assert!(!out.contains("command = \"h5i hook stop\""));
        assert!(out.contains("command = \"h5i msg hook\""));
        assert!(!out.contains("wrap-bash"));
        // UserPromptSubmit is Claude-only; Codex config must not carry it.
        assert!(!out.contains("UserPromptSubmit"));
        assert!(!out.contains("h5i hook claude prompt"));
    }

    #[test]
    fn codex_config_toml_wrap_bash_is_idempotent() {
        let once = merge_codex_config_toml("", true).unwrap();
        let twice = merge_codex_config_toml(&once, true).unwrap();
        assert_eq!(once, twice);
        assert!(once.contains("matcher = \"Bash\""));
        assert!(once.contains("command = \"h5i hook wrap-bash\""));
    }

    #[test]
    fn legacy_observe_bash_entry_is_always_stripped() {
        let existing = r#"{
            "hooks": {
                "PostToolUse": [
                    { "matcher": "Bash", "hooks": [ { "type": "command", "command": "h5i hook observe-bash" } ] }
                ]
            }
        }"#;
        let out = merge_hook_settings_json(existing, false).unwrap();
        assert!(!out.contains("observe-bash"));
    }

    #[test]
    fn legacy_agent_command_paths_are_migrated_in_place() {
        // An install wired before the `h5i claude/codex` → `h5i hook claude/codex`
        // rename. A re-run must swap each to the canonical path with no duplicate
        // left behind (a surviving alias entry would double-fire).
        let existing = r#"{
            "hooks": {
                "PostToolUse": [ { "matcher": "Edit|Write|Read", "hooks": [ { "type": "command", "command": "h5i claude sync" } ] } ],
                "Stop": [ { "hooks": [ { "type": "command", "command": "h5i claude finish" } ] } ],
                "UserPromptSubmit": [ { "hooks": [ { "type": "command", "command": "h5i claude prompt" } ] } ]
            }
        }"#;
        let out = merge_hook_settings_json(existing, false).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(commands_under(&v, "PostToolUse"), vec!["h5i hook claude sync"]);
        assert_eq!(commands_under(&v, "Stop"), vec!["h5i hook claude finish"]);
        assert_eq!(
            commands_under(&v, "UserPromptSubmit"),
            vec!["h5i hook claude prompt"]
        );
    }

    #[test]
    fn legacy_codex_stop_hook_is_migrated_in_place() {
        let existing = r#"
[[hooks.Stop]]
[[hooks.Stop.hooks]]
type = "command"
command = "h5i codex finish --quiet"
"#;
        let out = merge_codex_config_toml(existing, false).unwrap();
        assert!(out.contains("command = \"h5i hook codex finish --quiet\""));
        // The bare legacy path must be gone (only the new one, which has it as a
        // non-matching prefix, remains).
        assert!(!out.contains("\"h5i codex finish --quiet\""));
    }

    #[test]
    fn idempotent_under_reapplication() {
        let once = merge_hook_settings_json("", true).unwrap();
        let twice = merge_hook_settings_json(&once, true).unwrap();
        assert_eq!(once, twice);
    }

    #[test]
    fn preserves_unrelated_settings_and_msg_hook() {
        let existing = r#"{
            "env": { "H5I_AGENT": "claude" },
            "hooks": {
                "Stop": [ { "hooks": [ { "type": "command", "command": "h5i msg hook --block" } ] } ]
            }
        }"#;
        let out = merge_hook_settings_json(existing, false).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            v.pointer("/env/H5I_AGENT").and_then(|x| x.as_str()),
            Some("claude")
        );
        let stop = commands_under(&v, "Stop");
        assert!(stop.contains(&"h5i msg hook --block".to_string()));
        assert!(stop.contains(&"h5i hook claude finish".to_string()));
    }

    #[test]
    fn default_leaves_existing_wrap_bash_alone() {
        let with_wrap = merge_hook_settings_json("", true).unwrap();
        let out = merge_hook_settings_json(&with_wrap, false).unwrap();
        assert!(out.contains("h5i hook wrap-bash"));
    }

    #[test]
    fn rejects_non_object_settings() {
        assert!(merge_hook_settings_json("[1,2]", false).is_err());
        assert!(merge_hook_settings_json("\"nope\"", false).is_err());
    }

    #[test]
    fn command_match_is_exact_or_space_delimited() {
        let existing = r#"{
            "hooks": {
                "PostToolUse": [ { "matcher": "Bash", "hooks": [ { "type": "command", "command": "h5i hook run-custom" } ] } ]
            }
        }"#;
        let out = merge_hook_settings_json(existing, false).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let cmds = commands_under(&v, "PostToolUse");
        // The unrelated `run-custom` survives next to the managed `h5i hook claude sync`.
        assert!(cmds.contains(&"h5i hook run-custom".to_string()));
        assert!(cmds.contains(&"h5i hook claude sync".to_string()));
    }

    #[test]
    fn wrap_simple_command_keeps_real_argv() {
        assert_eq!(
            wrap_bash_command("cargo test --verbose").as_deref(),
            Some("h5i capture run -- cargo test --verbose")
        );
        assert_eq!(
            wrap_bash_command("  pytest -q tests/unit  ").as_deref(),
            Some("h5i capture run -- pytest -q tests/unit")
        );
    }

    #[test]
    fn wrap_shell_syntax_goes_through_bash_c() {
        assert_eq!(
            wrap_bash_command("cargo test 2>&1 | tail -5").as_deref(),
            Some("h5i capture run -- bash -c 'cargo test 2>&1 | tail -5'")
        );
        // Globs need a shell to expand.
        assert_eq!(
            wrap_bash_command("ls *.rs").as_deref(),
            Some("h5i capture run -- bash -c 'ls *.rs'")
        );
        // Embedded single quotes survive the '\'' encoding.
        assert_eq!(
            wrap_bash_command("echo 'a b'").as_deref(),
            Some(r#"h5i capture run -- bash -c 'echo '\''a b'\'''"#)
        );
        // A leading env assignment is shell syntax, not an executable.
        assert_eq!(
            wrap_bash_command("RUST_LOG=debug cargo run").as_deref(),
            Some("h5i capture run -- bash -c 'RUST_LOG=debug cargo run'")
        );
    }

    #[test]
    fn wrap_skips_h5i_cd_and_empty() {
        assert_eq!(wrap_bash_command("h5i recall object abc123"), None);
        assert_eq!(wrap_bash_command("/usr/local/bin/h5i msg inbox"), None);
        // A top-level cd must reach the session shell, in any position.
        assert_eq!(wrap_bash_command("cd /tmp"), None);
        assert_eq!(wrap_bash_command("git fetch && cd sub && make"), None);
        assert_eq!(wrap_bash_command("cd"), None);
        assert_eq!(wrap_bash_command("   "), None);
        // …but `cd` as an argument is not a top-level command.
        assert!(wrap_bash_command("grep -rn cd src/").is_some());
        // An hd-, cdx-style prefix is not cd.
        assert!(wrap_bash_command("cdk deploy").is_some());
    }
}
