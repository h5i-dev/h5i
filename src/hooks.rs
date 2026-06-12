//! Claude Code hook wiring — the pure settings.json merge behind
//! `h5i hook setup --write`.

use crate::error::H5iError;
use serde_json::{Map, Value};

/// The hook entries `h5i hook setup --write` manages, as
/// `(event, matcher, command)`. Bash observation is NOT here — it is opt-in
/// (`--observe-bash`) because it stores every Bash command + output as
/// evidence, which not every repo wants by default.
const CORE_HOOKS: &[(&str, Option<&str>, &str)] = &[
    ("SessionStart", None, "h5i hook session-start"),
    ("PostToolUse", Some("Edit|Write|Read"), "h5i hook run"),
    ("Stop", None, "h5i hook stop"),
];

/// The opt-in Bash observation entry.
const OBSERVE_BASH_HOOK: (&str, Option<&str>, &str) =
    ("PostToolUse", Some("Bash"), "h5i hook observe-bash");

/// Idempotently merge the h5i hook wiring into a Claude Code `settings.json`
/// document: SessionStart (context prelude), PostToolUse on Edit|Write|Read
/// (auto-trace), Stop (auto-checkpoint), and — only when `observe_bash` —
/// PostToolUse on Bash (`h5i hook observe-bash`). Each managed command is
/// replaced in place if already present; everything else (env keys, the
/// `h5i msg hook` Stop entry, user hooks) is preserved. Without
/// `observe_bash` an *existing* observe-bash entry is left alone — opting
/// out of adding it is not a request to remove it. `existing` may be empty
/// (treated as `{}`). Pure (no I/O) so it is unit-testable; the caller does
/// the file read/write.
pub fn merge_hook_settings_json(existing: &str, observe_bash: bool) -> Result<String, H5iError> {
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
        ensure_hook_entry(hooks_obj, event, matcher, command)?;
    }
    if observe_bash {
        let (event, matcher, command) = OBSERVE_BASH_HOOK;
        ensure_hook_entry(hooks_obj, event, matcher, command)?;
    }

    Ok(serde_json::to_string_pretty(&root)?)
}

/// Ensure `hooks.<event>` contains exactly one entry for `command`: drop any
/// prior entry carrying that command (so a re-run also refreshes the
/// matcher), then append `{ matcher?, hooks: [{type: command, command}] }`.
fn ensure_hook_entry(
    hooks_obj: &mut Map<String, Value>,
    event: &str,
    matcher: Option<&str>,
    command: &str,
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
    entry.insert(
        "hooks".to_string(),
        serde_json::json!([ { "type": "command", "command": command } ]),
    );
    arr.push(Value::Object(entry));
    Ok(())
}

/// True if a hooks-array entry contains an inner command that is `command`
/// (exactly, or followed by arguments). Exact-or-space matching so
/// `h5i hook run` never claims `h5i hook run-something-else`.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn commands_under(root: &Value, event: &str) -> Vec<String> {
        root.pointer(&format!("/hooks/{event}"))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .flat_map(|e| e.get("hooks").and_then(|h| h.as_array()).cloned().unwrap_or_default())
                    .filter_map(|hk| hk.get("command").and_then(|c| c.as_str()).map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn fresh_default_has_core_hooks_but_no_observe_bash() {
        let out = merge_hook_settings_json("", false).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(commands_under(&v, "SessionStart"), vec!["h5i hook session-start"]);
        assert_eq!(commands_under(&v, "PostToolUse"), vec!["h5i hook run"]);
        assert_eq!(commands_under(&v, "Stop"), vec!["h5i hook stop"]);
        assert!(!out.contains("observe-bash"));
        // The Edit|Write|Read matcher rides along with `h5i hook run`.
        assert_eq!(
            v.pointer("/hooks/PostToolUse/0/matcher").and_then(|m| m.as_str()),
            Some("Edit|Write|Read")
        );
    }

    #[test]
    fn observe_bash_flag_adds_bash_matcher_entry() {
        let out = merge_hook_settings_json("{}", true).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let cmds = commands_under(&v, "PostToolUse");
        assert!(cmds.contains(&"h5i hook run".to_string()));
        assert!(cmds.contains(&"h5i hook observe-bash".to_string()));
        let bash_entry = v
            .pointer("/hooks/PostToolUse")
            .and_then(|a| a.as_array())
            .unwrap()
            .iter()
            .find(|e| entry_has_command(e, "h5i hook observe-bash"))
            .unwrap();
        assert_eq!(bash_entry.get("matcher").and_then(|m| m.as_str()), Some("Bash"));
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
        assert_eq!(v.pointer("/env/H5I_AGENT").and_then(|x| x.as_str()), Some("claude"));
        let stop = commands_under(&v, "Stop");
        assert!(stop.contains(&"h5i msg hook --block".to_string()));
        assert!(stop.contains(&"h5i hook stop".to_string()));
    }

    #[test]
    fn default_leaves_existing_observe_bash_alone() {
        let with_bash = merge_hook_settings_json("", true).unwrap();
        let out = merge_hook_settings_json(&with_bash, false).unwrap();
        assert!(out.contains("h5i hook observe-bash"));
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
        // The unrelated `run-custom` survives next to the managed `h5i hook run`.
        assert!(cmds.contains(&"h5i hook run-custom".to_string()));
        assert!(cmds.contains(&"h5i hook run".to_string()));
    }
}
