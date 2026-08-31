//! Browser evidence: collecting what the page said after the agent touched it.
//!
//! `h5i box run <box> -- agent-browser click @e2` already produces a receipt for
//! the command. What it does not produce, and what a reviewer needs, is the
//! page's answer: the console error the click raised, the exception it threw,
//! the request that came back 500. Without those the browser half of an export
//! is the agent's own testimony.
//!
//! So h5i drains them itself, right after the run, in the same box under the
//! same policy. Three properties fall out of doing it here:
//!
//! * Timing is ours. The drain happens at a point the agent does not choose, so
//!   evidence cannot be collected only when it is flattering.
//! * Scope is ours. The cursor lives host-side, outside every write grant the
//!   box has (roadmap 5.7), so a record already written cannot be walked back.
//! * Absence is visible. A run that invoked a browser verb and produced no
//!   evidence block is marked `unavailable`, not rendered as a clean page.
//!
//! What this is not: a containment boundary. The strings come from a browser
//! inside the box, so the lane is box-claimed and the docs say so.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::receipt::BrowserEvidence;

/// The binary whose invocations are browser commands.
const DRIVER: &str = "agent-browser";

/// Per-record cap on evidence lines of each kind. A page in a redirect loop can
/// produce thousands of failed requests, and a receipt is meant to be read.
const MAX_LINES: usize = 40;

/// Longest single evidence line kept. Stack traces and data: URLs are unbounded.
const MAX_LINE: usize = 500;

/// Where the drain cursor lives, relative to an env directory. Host-side, and
/// deliberately not under `<env>/spool`, the box's only write window there,
/// so the box cannot rewind it to make old evidence look new, or advance it to
/// make new evidence disappear.
const CURSOR_FILE: &str = "browser-cursor.json";

/// How much of each browser buffer has already been recorded.
///
/// The buffers accumulate for the life of a browser session and `--clear` is a
/// separate round trip that does not return what it clears, so a cursor is both
/// cheaper and more honest than clearing: nothing is destroyed, and each record
/// carries only its own slice.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
struct Cursor {
    /// Which browser session these counts belong to. See [`session_id`].
    ///
    /// Without this the cursor outlives the thing it indexes. At the supervised
    /// tier every run gets a fresh network namespace and a fresh browser, so
    /// counts from the previous run index a buffer that no longer exists, and a
    /// new session that happens to produce the *same number* of messages gets
    /// silently reported as clean. Counting backwards catches a shorter buffer;
    /// only an identity catches an equal one.
    #[serde(default)]
    session: Option<String>,
    console: usize,
    errors: usize,
    requests: usize,
}

/// Identity of the browser session currently running in this box.
///
/// The daemon writes `<session>.pid` next to its socket, and the private `/tmp`
/// that holds both is wiped at the start of every run, so the set of pid files
/// is a serviceable session fingerprint, readable from the host without asking
/// the box anything.
pub(crate) fn session_id(tmp_root: &Path) -> Option<String> {
    // Both directories: under mediation the daemon's pid file is in the private
    // one and the shim mirrors only .version/.config/.stream, so looking at the
    // visible directory alone returns None on every mediated run, and a cursor
    // that never sees a session change never resets.
    //
    // Bounded in both directions, because this walks `<env>/tmp`, one of the two
    // paths a box can write, from the *host*. A pid file holds a decimal
    // integer; without a cap, a box could put four gigabytes in one and have the
    // host allocate it, or fill the directory and have the host build an
    // unbounded `ids` vector out of the names.
    const MAX_PID_FILES: usize = 64;
    const MAX_PID_BYTES: u64 = 64;
    let mut ids: Vec<String> = std::fs::read_dir(daemon_dir(tmp_root))
        .into_iter()
        .flatten()
        .chain(std::fs::read_dir(socket_dir(tmp_root)).into_iter().flatten())
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".pid"))
        .take(MAX_PID_FILES)
        .filter_map(|e| {
            use std::io::Read;
            let mut pid = String::new();
            std::fs::File::open(e.path())
                .ok()?
                .take(MAX_PID_BYTES)
                .read_to_string(&mut pid)
                .ok()?;
            Some(format!(
                "{}:{}",
                e.file_name().to_string_lossy(),
                pid.trim()
            ))
        })
        .collect();
    if ids.is_empty() {
        return None;
    }
    ids.sort();
    Some(ids.join(","))
}

fn cursor_path(env_dir: &Path) -> PathBuf {
    env_dir.join(CURSOR_FILE)
}

fn read_cursor(env_dir: &Path) -> Cursor {
    std::fs::read(cursor_path(env_dir))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn write_cursor(env_dir: &Path, c: &Cursor) {
    if let Ok(bytes) = serde_json::to_vec(c) {
        let _ = std::fs::write(cursor_path(env_dir), bytes);
    }
}

/// Host path of the box's agent-browser socket directory.
///
/// `browser_env` points the daemon at `/tmp/agent-browser`, and at the kernel
/// tiers `/tmp` is the per-env scratch backed by `<env>/tmp`, so the socket the
/// box created is visible from here. That is what makes the "is there a browser
/// to ask?" check free: no exec, no daemon launch, just a directory listing.
///
/// Takes the root rather than deriving `<env>/tmp` itself: that derivation is
/// only correct on Linux kernel tiers, and getting it wrong means the liveness
/// gate is always false and the evidence drain is silently skipped, so a run
/// looks clean because nothing was ever collected. `env::host_tmp_root` is the
/// single place that knows the mapping.
fn socket_dir(tmp_root: &Path) -> PathBuf {
    tmp_root.join("agent-browser")
}

/// Directory (under the box's `/tmp`) where the shim starts the real daemon
/// when h5i is mediating the socket the box is told to use.
pub(crate) const DAEMON_DIR_NAME: &str = "agent-browser-daemon";

fn daemon_dir(tmp_root: &Path) -> PathBuf {
    tmp_root.join(DAEMON_DIR_NAME)
}

/// Is a browser actually running in this box right now?
///
/// This gate exists because the drain command *starts* a browser if none is
/// running. Without it, a `cargo build` in a browser box would launch Chrome
/// purely to be told the console was empty, and every run would report a
/// spuriously clean page.
///
/// A session is a socket *and* a pid file together, in one directory. Either
/// alone is a leftover: a stale `.pid` outlives a daemon that died, and since
/// h5i began mediating, a `.sock` in the box-visible directory may be h5i's own
/// listener, bound before the box starts, which would make this gate
/// unconditionally true. Only a running daemon leaves both.
///
/// Both directories are searched, because under mediation the real daemon binds
/// the private one while an unmediated box still uses the visible one.
pub fn browser_is_live(tmp_root: &Path) -> bool {
    [socket_dir(tmp_root), daemon_dir(tmp_root)]
        .iter()
        .any(|dir| {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return false;
            };
            let (mut sock, mut pid) = (false, false);
            for name in entries.flatten().map(|e| e.file_name()) {
                let name = name.to_string_lossy();
                sock |= name.ends_with(".sock");
                pid |= name.ends_with(".pid");
            }
            sock && pid
        })
}

/// Did this run touch the browser at all?
///
/// Two shapes count. A direct invocation is the obvious one. The other is the
/// one agents actually produce: `sh -c '... && agent-browser click @e2'`, where
/// argv[0] is a shell and the driver is buried in the command string. Matching
/// the string is a heuristic, and it is the right kind of wrong: it can
/// over-fire, and over-firing costs one exec that [`browser_is_live`] has
/// already established is worth making.
pub fn run_touched_browser(argv: &[String]) -> Option<String> {
    if let Some(verb) = command_verb(argv) {
        return Some(verb);
    }
    argv.iter()
        .any(|a| a.contains(DRIVER))
        .then(|| "shell".to_string())
}

/// The `agent-browser` verb this command runs, if it is one.
///
/// Matched on the basename so an absolute path counts, and the verb is the first
/// argument that is not a global flag: `agent-browser --session x open URL` is
/// an `open`. Returns `None` for anything else, which is how a `cargo test` in a
/// browser box avoids paying for a drain it has no use for.
pub fn command_verb(argv: &[String]) -> Option<String> {
    let base = Path::new(argv.first()?).file_name()?.to_str()?;
    if base != DRIVER {
        return None;
    }
    let mut rest = argv[1..].iter();
    while let Some(a) = rest.next() {
        if let Some(flag) = a.strip_prefix("--") {
            // The global flags that take a value; skip the value with them so
            // `--session probe open` reports `open`, not `probe`.
            if matches!(flag, "session" | "profile" | "state" | "engine" | "provider") {
                rest.next();
            }
            continue;
        }
        if a.starts_with('-') {
            continue;
        }
        return Some(a.clone());
    }
    // A bare `agent-browser` with no verb prints help. Still a browser command,
    // just not one that touched a page.
    Some(DRIVER.to_string())
}

/// Does this verb warrant a drain? Everything that can change or inspect a page
/// does; the session-management verbs do not, and `close` especially must not.
/// Draining after it would relaunch the browser we were just asked to shut down.
pub fn verb_wants_drain(verb: &str) -> bool {
    !matches!(
        verb,
        "close" | "install" | "skills" | "upgrade" | "doctor" | DRIVER
    )
}

/// The single in-box command that collects everything, in one exec.
///
/// `batch` matters here: three separate invocations would be three confined
/// spawns and three daemon round trips on every browser command the agent runs.
pub fn drain_argv() -> Vec<String> {
    [
        DRIVER,
        "--json",
        "batch",
        "console",
        "errors",
        "network requests",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn clip(s: &str) -> String {
    let one_line = s.replace(['\n', '\r'], " ");
    if one_line.chars().count() <= MAX_LINE {
        return one_line;
    }
    let head: String = one_line.chars().take(MAX_LINE).collect();
    format!("{head}…")
}

/// Parse the `batch` output into the slice of each buffer past `cursor`, and
/// advance the cursor.
///
/// Tolerant by construction: this is third-party JSON on the critical path of a
/// run that has already succeeded, so a shape we do not recognize yields empty
/// evidence rather than failing the run. The one thing it must not do is
/// silently report a clean page when it could not read one, which is what
/// `unavailable` is for.
fn parse_batch(out: &[u8], cursor: &mut Cursor) -> BrowserEvidence {
    let mut ev = BrowserEvidence::default();
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(out) else {
        return ev;
    };
    let Some(entries) = v.as_array() else {
        return ev;
    };

    // `Cell`, because both closures below need to set it and the borrow
    // checker will not hand the same `&mut bool` to two of them.
    let truncated = std::cell::Cell::new(false);
    // Take everything past the cursor, capped. A buffer shorter than the cursor
    // means the session restarted and the numbering began again, so the whole
    // buffer is new.
    let slice = |items: &[serde_json::Value], seen: &mut usize| -> Vec<serde_json::Value> {
        let start = if items.len() < *seen { 0 } else { *seen };
        *seen = items.len();
        let fresh = &items[start..];
        if fresh.len() > MAX_LINES {
            truncated.set(true);
            fresh[fresh.len() - MAX_LINES..].to_vec()
        } else {
            fresh.to_vec()
        }
    };

    // `slice` caps each *array*, and the outer list has no cap at all, so a
    // batch of ten thousand entries, each carrying its own `messages` array,
    // multiplied straight through into `ev` and from there into a receipt line.
    // The box writes this JSON, so the total is what has to be bounded, not the
    // per-array slice. Kept as a closure over the three vectors so the rule is
    // stated once and cannot apply to two of the three.
    let keep = |v: &mut Vec<String>, line: String| {
        if v.len() >= MAX_LINES {
            truncated.set(true);
            return;
        }
        v.push(line);
    };

    for entry in entries {
        let result = &entry["result"];
        if let Some(msgs) = result["messages"].as_array() {
            for m in slice(msgs, &mut cursor.console) {
                // Only the levels that mean something went wrong. `log` and
                // `info` are the page talking to itself.
                let level = m["type"].as_str().unwrap_or("");
                if level != "error" && level != "warning" {
                    continue;
                }
                let text = m["text"].as_str().unwrap_or_default();
                keep(&mut ev.console, clip(&format!("[{level}] {text}")));
            }
        }
        if let Some(errs) = result["errors"].as_array() {
            for e in slice(errs, &mut cursor.errors) {
                keep(&mut ev.errors, clip(e["text"].as_str().unwrap_or_default()));
            }
        }
        if let Some(reqs) = result["requests"].as_array() {
            for r in slice(reqs, &mut cursor.requests) {
                // No status means the request never completed: blocked by the
                // egress allowlist, refused, or aborted. In a box that is the
                // most interesting failure of the three, so it is not skipped.
                let status = r["status"].as_u64();
                let failed = status.map(|s| s >= 400).unwrap_or(true);
                if !failed {
                    continue;
                }
                let method = r["method"].as_str().unwrap_or("?");
                let url = r["url"].as_str().unwrap_or_default();
                let shown = status
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "no response".into());
                keep(&mut ev.failed_requests, clip(&format!("{shown} {method} {url}")));
            }
        }
    }
    ev.truncated = truncated.get();
    ev
}

/// Collect evidence for a run that drove the browser.
///
/// `run` executes the drain inside the box under the run's own policy and
/// returns its stdout; the caller owns that because only it holds the resolved
/// policy and the injected environment. `None` from `run` means the drain could
/// not execute at all, which is recorded as `unavailable` rather than as a clean
/// page.
pub fn collect(
    env_dir: &Path,
    tmp_root: &Path,
    verb: &str,
    run: impl FnOnce(&[String]) -> Option<Vec<u8>>,
) -> BrowserEvidence {
    // `env_dir` holds the host-owned cursor; `tmp_root` is where the browser's
    // sockets actually are, which is not `<env>/tmp` on every tier.
    let session = session_id(tmp_root);
    let mut cursor = read_cursor(env_dir);
    // A different browser than the one these counts came from: start over, or
    // this session's findings are measured against a buffer that never existed.
    if cursor.session != session {
        cursor = Cursor {
            session: session.clone(),
            ..Default::default()
        };
    }
    let Some(out) = run(&drain_argv()) else {
        return BrowserEvidence {
            verb: Some(verb.to_string()),
            unavailable: true,
            ..Default::default()
        };
    };
    let mut ev = parse_batch(&out, &mut cursor);
    ev.verb = Some(verb.to_string());
    write_cursor(env_dir, &cursor);
    ev
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mediator_socket_alone_does_not_count_as_a_live_browser() {
        // h5i binds its listener in this directory before the box starts, so a
        // socket-based check would be unconditionally true and the drain would
        // launch Chrome purely to report an empty console. The spurious launch
        // this gate exists to prevent.
        let td = tempfile::tempdir().expect("tempdir");
        let dir = td.path().join("tmp").join("agent-browser");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("default.sock"), b"").unwrap();
        assert!(
            !browser_is_live(&td.path().join("tmp")),
            "a socket with no pid beside it is h5i's listener, not a browser"
        );

        // A real daemon writes a pid file.
        std::fs::write(dir.join("default.pid"), b"1234").unwrap();
        assert!(browser_is_live(&td.path().join("tmp")));
    }

    #[test]
    fn a_daemon_on_the_mediated_private_path_still_counts_as_live() {
        // Under mediation the real daemon lives in the private directory, so
        // looking only at the box-visible one would report every mediated
        // session as having no browser.
        let td = tempfile::tempdir().expect("tempdir");
        let dir = td.path().join("tmp").join(DAEMON_DIR_NAME);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("default.sock"), b"").unwrap();
        std::fs::write(dir.join("default.pid"), b"4321").unwrap();
        assert!(browser_is_live(&td.path().join("tmp")));
    }

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn only_agent_browser_invocations_are_browser_commands() {
        assert_eq!(command_verb(&argv(&["agent-browser", "open", "u"])).as_deref(), Some("open"));
        // Absolute path still counts. The box may not resolve it through PATH.
        assert_eq!(
            command_verb(&argv(&["/usr/local/bin/agent-browser", "snapshot"])).as_deref(),
            Some("snapshot")
        );
        // Global flags are skipped, including the ones that take a value: the
        // verb is `open`, not the session name.
        assert_eq!(
            command_verb(&argv(&["agent-browser", "--session", "probe", "open", "u"])).as_deref(),
            Some("open")
        );
        assert_eq!(
            command_verb(&argv(&["agent-browser", "--json", "click", "@e2"])).as_deref(),
            Some("click")
        );
        // Everything else pays nothing.
        assert!(command_verb(&argv(&["cargo", "test"])).is_none());
        assert!(command_verb(&argv(&["npm", "run", "agent-browser"])).is_none());
        assert!(command_verb(&[]).is_none());
    }

    #[test]
    fn a_shell_wrapped_browser_command_still_counts() {
        // The shape agents actually produce: the driver is inside the command
        // string, not at argv[0]. Missing these would leave the common case
        // with no evidence at all.
        assert_eq!(
            run_touched_browser(&argv(&["sh", "-c", "npm run dev & agent-browser open /"])).as_deref(),
            Some("shell")
        );
        // A direct invocation still reports its real verb, which is more useful
        // than "shell".
        assert_eq!(
            run_touched_browser(&argv(&["agent-browser", "click", "@e2"])).as_deref(),
            Some("click")
        );
        // And a run with nothing to do with the browser pays nothing.
        assert!(run_touched_browser(&argv(&["cargo", "test"])).is_none());
    }

    #[test]
    fn no_socket_means_no_browser_to_ask() {
        let td = tempfile::tempdir().unwrap();
        // The gate that keeps the drain from *starting* a browser: with no
        // session, a run must not be recorded as having seen a clean page.
        assert!(!browser_is_live(&td.path().join("tmp")));

        let sockets = td.path().join("tmp").join("agent-browser");
        std::fs::create_dir_all(&sockets).unwrap();
        assert!(!browser_is_live(&td.path().join("tmp")), "an empty socket dir is not a session");
        std::fs::write(sockets.join("default.pid"), "123").unwrap();
        assert!(!browser_is_live(&td.path().join("tmp")), "a stale pid file is not a socket");
        std::fs::write(sockets.join("default.sock"), "").unwrap();
        assert!(browser_is_live(&td.path().join("tmp")));
    }

    #[test]
    fn close_is_not_drained() {
        // Draining after `close` would relaunch the browser the run just shut
        // down, which is both wrong and expensive.
        assert!(!verb_wants_drain("close"));
        assert!(!verb_wants_drain("doctor"));
        assert!(verb_wants_drain("open"));
        assert!(verb_wants_drain("click"));
    }

    fn batch_json(console: &str, errors: &str, requests: &str) -> Vec<u8> {
        format!(
            r#"[{{"command":["console"],"result":{{"messages":[{console}]}}}},
                {{"command":["errors"],"result":{{"errors":[{errors}]}}}},
                {{"command":["network","requests"],"result":{{"requests":[{requests}]}}}}]"#
        )
        .into_bytes()
    }

    #[test]
    fn evidence_is_the_signal_not_the_chatter() {
        let mut c = Cursor::default();
        let out = batch_json(
            r#"{"type":"log","text":"starting up"},{"type":"error","text":"boom"}"#,
            r#"{"text":"TypeError: x is null"}"#,
            r#"{"status":200,"method":"GET","url":"/ok"},
               {"status":500,"method":"POST","url":"/api/save"},
               {"method":"GET","url":"https://blocked.example"}"#,
        );
        let ev = parse_batch(&out, &mut c);

        // `log` is the page talking to itself; `error` is evidence.
        assert_eq!(ev.console, vec!["[error] boom"]);
        assert_eq!(ev.errors, vec!["TypeError: x is null"]);
        // A 200 is not a finding. A 500 is, and so is a request that never got
        // a response at all, which in a box usually means the egress allowlist.
        assert_eq!(
            ev.failed_requests,
            vec!["500 POST /api/save", "no response GET https://blocked.example"]
        );
        assert!(!ev.is_clean());
    }

    #[test]
    fn the_cursor_means_each_record_carries_only_its_own_slice() {
        let mut c = Cursor::default();
        let first = parse_batch(
            &batch_json(r#"{"type":"error","text":"one"}"#, "", ""),
            &mut c,
        );
        assert_eq!(first.console, vec!["[error] one"]);

        // The buffer accumulates; the second drain must not repeat "one".
        let second = parse_batch(
            &batch_json(
                r#"{"type":"error","text":"one"},{"type":"error","text":"two"}"#,
                "",
                "",
            ),
            &mut c,
        );
        assert_eq!(second.console, vec!["[error] two"]);

        // A shorter buffer means the session restarted and numbering began
        // again. Take it all rather than silently reporting nothing.
        let restarted = parse_batch(
            &batch_json(r#"{"type":"error","text":"fresh"}"#, "", ""),
            &mut c,
        );
        assert_eq!(restarted.console, vec!["[error] fresh"]);
    }

    #[test]
    fn a_flood_is_capped_and_says_so() {
        let many: Vec<String> = (0..MAX_LINES + 10)
            .map(|i| format!(r#"{{"status":500,"method":"GET","url":"/r{i}"}}"#))
            .collect();
        let mut c = Cursor::default();
        let ev = parse_batch(&batch_json("", "", &many.join(",")), &mut c);
        assert_eq!(ev.failed_requests.len(), MAX_LINES);
        assert!(ev.truncated, "a capped record must say it was capped");
        // The tail is what is kept: the most recent failures are the ones that
        // explain the state the run ended in.
        assert!(ev.failed_requests.last().unwrap().ends_with("/r49"));
    }

    /// The per-array cap bounded one `messages` list; the outer batch had none,
    /// so a box returning ten thousand entries multiplied straight through into
    /// the evidence and from there into a receipt line. The box writes this
    /// JSON, so the *total* is what has to be bounded.
    #[test]
    fn a_flood_spread_across_many_entries_is_capped_too() {
        // Each entry's buffer is one longer than the last, which is what an
        // accumulating buffer actually looks like, so the cursor advances by
        // one per entry and every entry contributes a fresh line.
        let entries: Vec<String> = (0..200)
            .map(|i| {
                let errs: Vec<String> = (0..=i)
                    .map(|j| format!(r#"{{"text":"boom {j}"}}"#))
                    .collect();
                format!(
                    r#"{{"result":{{"errors":[{}],"messages":[],"requests":[]}}}}"#,
                    errs.join(",")
                )
            })
            .collect();
        let batch = format!("[{}]", entries.join(","));

        let mut c = Cursor::default();
        let ev = parse_batch(batch.as_bytes(), &mut c);
        assert_eq!(ev.errors.len(), MAX_LINES, "the total must be bounded");
        assert!(ev.truncated, "a capped record must say it was capped");
    }

    #[test]
    fn unparseable_output_is_empty_evidence_not_a_failed_run() {
        // Third-party JSON on the path of a run that already succeeded. A shape
        // we do not know must not turn a green run red.
        let mut c = Cursor::default();
        assert_eq!(parse_batch(b"not json at all", &mut c), BrowserEvidence::default());
        assert_eq!(parse_batch(b"{}", &mut c), BrowserEvidence::default());
        assert_eq!(c, Cursor::default(), "a failed parse must not move the cursor");
    }

    #[test]
    fn a_missing_browser_is_recorded_as_unavailable_not_as_clean() {
        let td = tempfile::tempdir().unwrap();
        let ev = collect(td.path(), &td.path().join("tmp"), "click", |_| None);
        assert!(ev.unavailable);
        // The distinction a reviewer needs: nothing was looked at, which is not
        // the same claim as nothing went wrong.
        assert!(ev.is_clean());
        assert_eq!(ev.verb.as_deref(), Some("click"));
    }

    /// `session_id` walks `<env>/tmp`, one of the two paths a box can write,
    /// from the host, and read every `.pid` file whole. A pid file holds a
    /// decimal integer; a box could put four gigabytes in one, or fill the
    /// directory, and the host would allocate it either way.
    #[test]
    fn a_hostile_pid_file_is_not_a_session_fingerprint() {
        let td = tempfile::tempdir().unwrap();
        let tmp_root = td.path().join("tmp");
        let sockets = tmp_root.join("agent-browser");
        std::fs::create_dir_all(&sockets).unwrap();

        std::fs::write(sockets.join("huge.pid"), "9".repeat(4 * 1024 * 1024)).unwrap();
        for i in 0..300 {
            std::fs::write(sockets.join(format!("f{i}.pid")), "1001").unwrap();
        }

        let id = session_id(&tmp_root).expect("some fingerprint");
        assert!(
            id.len() < 16 * 1024,
            "the fingerprint grew to {} bytes",
            id.len()
        );

        // ...and an ordinary session still fingerprints as it did.
        let td = tempfile::tempdir().unwrap();
        let sockets = td.path().join("tmp").join("agent-browser");
        std::fs::create_dir_all(&sockets).unwrap();
        std::fs::write(sockets.join("default.pid"), "1001\n").unwrap();
        assert_eq!(
            session_id(&td.path().join("tmp")).as_deref(),
            Some("default.pid:1001")
        );
    }

    #[test]
    fn a_new_browser_session_resets_the_cursor() {
        let td = tempfile::tempdir().unwrap();
        let sockets = td.path().join("tmp").join("agent-browser");
        std::fs::create_dir_all(&sockets).unwrap();
        std::fs::write(sockets.join("default.sock"), "").unwrap();
        std::fs::write(sockets.join("default.pid"), "1001").unwrap();

        let out = batch_json(r#"{"type":"error","text":"boom"}"#, "", "");
        assert_eq!(
            collect(td.path(), &td.path().join("tmp"), "open", |_| Some(out.clone())).console,
            vec!["[error] boom"]
        );
        // Same session, same buffer: already reported.
        assert!(collect(td.path(), &td.path().join("tmp"), "click", |_| Some(out.clone()))
            .console
            .is_empty());

        // New run at the supervised tier means a new netns and a new daemon.
        // The buffer is a *different* buffer that happens to be the same
        // length, so a count comparison alone would call this clean, which is
        // exactly the failure this identity exists to prevent.
        std::fs::write(sockets.join("default.pid"), "2002").unwrap();
        assert_eq!(
            collect(td.path(), &td.path().join("tmp"), "open", |_| Some(out.clone())).console,
            vec!["[error] boom"]
        );
    }

    #[test]
    fn the_cursor_persists_across_runs_and_lives_outside_the_spool() {
        let td = tempfile::tempdir().unwrap();
        let out = batch_json(r#"{"type":"error","text":"one"}"#, "", "");
        let first = collect(td.path(), &td.path().join("tmp"), "open", |_| Some(out.clone()));
        assert_eq!(first.console, vec!["[error] one"]);

        // Same buffer, new run: already recorded, so nothing new.
        let second = collect(td.path(), &td.path().join("tmp"), "click", |_| Some(out.clone()));
        assert!(second.console.is_empty());

        // And the cursor is a sibling of the spool, never inside it. The box's
        // write window must not include the thing that decides what is new.
        let cursor = cursor_path(td.path());
        assert!(cursor.is_file());
        assert!(!cursor.starts_with(td.path().join("spool")));
    }
}
