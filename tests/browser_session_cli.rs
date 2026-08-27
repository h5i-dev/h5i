//! `h5i browser` end to end, against the real engine.
//!
//! These drive the binary rather than the library, because the properties they
//! pin are properties of the command an agent actually types: that a dead
//! session is refused with its own exit code, that an id is never handed back
//! to a second session, and that what a page composed does not reach a terminal
//! with its escape sequences intact.
//!
//! Skipped, loudly, when there is no engine to drive. `H5I_BROWSER_ENGINE`
//! names one; otherwise a sibling of the test binary or `$PATH`.

use std::path::PathBuf;
use std::process::Command;

/// Exit status for a verb sent to a session that is not live. Copied rather
/// than imported so a change to the constant has to be made in two places, one
/// of which is a test that says why it matters.
const EXIT_SESSION_GONE: i32 = 69;

fn engine() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("H5I_BROWSER_ENGINE") {
        let path = PathBuf::from(explicit);
        return path.exists().then_some(path);
    }
    let mut here = std::env::current_exe().ok()?;
    here.pop();
    here.pop();
    let sibling = here.join("h5i-browser-light");
    sibling.exists().then_some(sibling)
}

fn h5i() -> PathBuf {
    let mut here = std::env::current_exe().expect("test binary path");
    here.pop();
    here.pop();
    here.join("h5i")
}

/// A one-page HTTP server, so the tests exercise the path the product is for.
///
/// `file://` was the obvious shortcut and the wrong one: the engine loads a
/// local file as a *start target* and refuses to **fetch** one, so a second
/// `open` on a file URL is denied by policy. That is correct behaviour (a
/// page-initiated navigation to `file:` is an exfiltration path) and it makes
/// file URLs unable to test navigation at all.
struct Site {
    base: String,
}

impl Site {
    /// Serve until the process ends. One thread, one connection at a time,
    /// which is all any of these tests needs.
    fn start() -> Option<Site> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").ok()?;
        let port = listener.local_addr().ok()?.port();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let _ = Site::serve_one(stream);
            }
        });
        Some(Site {
            base: format!("http://127.0.0.1:{port}"),
        })
    }

    fn serve_one(mut stream: std::net::TcpStream) -> std::io::Result<()> {
        use std::io::{BufRead, BufReader, Write};
        let mut line = String::new();
        BufReader::new(stream.try_clone()?).read_line(&mut line)?;
        let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
        let body = match path.as_str() {
            "/two" => "<html><head><title>Two</title></head><body><h1>second</h1></body></html>"
                .to_string(),
            // A title carrying escape sequences: a page repainting the terminal
            // it is printed into.
            "/hostile" => "<html><body><h1>start\u{1b}[2K\u{1b}[1Aoverwritten</h1></body></html>"
                .to_string(),
            _ => "<html><head><title>t</title></head><body><h1>hello</h1>\
                  <p>a <a href=\"https://example.com/next\">link</a></p></body></html>"
                .to_string(),
        };
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )?;
        stream.flush()
    }
}

struct Fixture {
    home: tempfile::TempDir,
    engine: PathBuf,
    site: Site,
}

impl Fixture {
    fn new() -> Option<Fixture> {
        let engine = engine()?;
        let h5i = h5i();
        if !h5i.exists() {
            return None;
        }
        let home = tempfile::tempdir().ok()?;
        let site = Site::start()?;
        Some(Fixture { home, engine, site })
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        Command::new(h5i())
            .args(args)
            .env("H5I_BROWSER_HOME", self.home.path())
            .env("H5I_BROWSER_ENGINE", &self.engine)
            .output()
            .expect("h5i runs")
    }

    /// The session's allowlist has to name the test server, because the engine
    /// reaches nothing it was not granted.
    fn open(&self, extra: &[&str]) -> String {
        let url = self.site.base.clone();
        let mut args = vec!["browser", "open", url.as_str(), "--allow", "127.0.0.1", "--json"];
        args.extend_from_slice(extra);
        let out = self.run(&args);
        assert!(
            out.status.success(),
            "open failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let record: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("open prints a record");
        record["id"].as_str().expect("an id").to_string()
    }

    fn dir(&self, id: &str) -> PathBuf {
        self.home.path().join("sessions").join(id)
    }
}

fn skip(why: &str) {
    eprintln!("skipping: {why}");
}

/// The shape an agent actually types. No id anywhere.
#[test]
fn the_ordinary_case_names_no_session() {
    let Some(fx) = Fixture::new() else {
        return skip("no h5i-browser-light to drive");
    };
    let url = fx.site.base.clone();
    assert!(fx.run(&["browser", "open", &url, "--allow", "127.0.0.1"]).status.success());

    let snapshot = fx.run(&["browser", "snapshot"]);
    assert!(
        snapshot.status.success(),
        "a bare verb must land on the session `open` just made: {}",
        String::from_utf8_lossy(&snapshot.stderr)
    );
    assert!(String::from_utf8_lossy(&snapshot.stdout).contains("hello"));

    assert!(fx.run(&["browser", "requests"]).status.success());
    assert!(fx.run(&["browser", "close"]).status.success());

    // And once it is closed, a bare verb says so rather than guessing.
    let after = fx.run(&["browser", "snapshot"]);
    assert_eq!(after.status.code(), Some(EXIT_SESSION_GONE));
    let why = String::from_utf8_lossy(&after.stderr);
    assert!(why.contains("h5i browser open"), "{why}");
}

/// A second `open` moves the session it finds. Forking silently would leave the
/// first one holding a page nothing points at.
#[test]
fn opening_again_navigates_rather_than_forking() {
    let Some(fx) = Fixture::new() else {
        return skip("no h5i-browser-light to drive");
    };
    let url = fx.site.base.clone();
    fx.open(&[]);
    // No `--allow` on the second open: the policy is already fixed, and passing
    // it again is the thing `a_creation_flag_on_a_live_session_is_refused`
    // pins. This is the plain "go there" case.
    assert!(fx.run(&["browser", "open", &url]).status.success());

    let listed = fx.run(&["browser", "list", "--json"]);
    let listed: Vec<serde_json::Value> = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(listed.len(), 1, "a second open forked the session");

    // `--new` is how you say you meant a second one.
    assert!(fx.run(&["browser", "open", &url, "--allow", "127.0.0.1", "--new"]).status.success());
    let listed = fx.run(&["browser", "list", "--json"]);
    let listed: Vec<serde_json::Value> = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(listed.len(), 2);
}

/// Names are for running several at once, and the id still addresses one.
#[test]
fn a_name_addresses_a_session_and_so_does_its_id() {
    let Some(fx) = Fixture::new() else {
        return skip("no h5i-browser-light to drive");
    };
    let auth = fx.open(&["--session", "auth"]);
    let public = fx.open(&["--session", "public", "--new"]);
    assert_ne!(auth, public);

    assert!(
        fx.run(&["browser", "status", "--session", "auth"])
            .status
            .success()
    );
    let by_id = fx.run(&["browser", "status", "--session", &auth, "--json"]);
    assert!(by_id.status.success());
    let record: serde_json::Value = serde_json::from_slice(&by_id.stdout).unwrap();
    assert_eq!(record["id"].as_str().unwrap(), auth);
    assert_eq!(record["name"].as_str().unwrap(), "auth");
}

/// A session's policy is fixed when its engine starts, so a flag that would
/// widen it is refused rather than accepted and ignored.
#[test]
fn a_creation_flag_on_a_live_session_is_refused() {
    let Some(fx) = Fixture::new() else {
        return skip("no h5i-browser-light to drive");
    };
    fx.open(&[]);
    let url = fx.site.base.clone();
    let out = fx.run(&["browser", "open", &url, "--allow", "example.com"]);
    assert!(!out.status.success());
    let why = String::from_utf8_lossy(&out.stderr);
    assert!(why.contains("--allow"), "{why}");
    assert!(why.contains("--new"), "the refusal names the way forward: {why}");
}

/// A verb the session refused must not exit 0. A script that checks the status
/// code would otherwise read "denied by policy" as success.
#[test]
fn a_refused_verb_exits_non_zero() {
    let Some(fx) = Fixture::new() else {
        return skip("no h5i-browser-light to drive");
    };
    fx.open(&[]);
    assert!(fx.run(&["browser", "snapshot"]).status.success());
    // The page's only link leaves the session's allowlist.
    let out = fx.run(&["browser", "click", "@e1"]);
    assert!(!out.status.success(), "a policy denial exited 0");
    let why = String::from_utf8_lossy(&out.stderr);
    assert!(why.contains("allowlist"), "{why}");
}

#[test]
fn a_session_starts_answers_and_closes() {
    let Some(fx) = Fixture::new() else {
        return skip("no h5i-browser-light to drive");
    };
    let id = fx.open(&[]);

    let snapshot = fx.run(&["browser", "snapshot"]);
    assert!(snapshot.status.success());
    let text = String::from_utf8_lossy(&snapshot.stdout);
    assert!(text.contains("hello"), "{text}");

    // The engine fences page content for a model. h5i must not undo that.
    assert!(text.contains("UNTRUSTED"), "{text}");

    let closed = fx.run(&["browser", "close"]);
    assert!(closed.status.success());

    // The record outlives the session: that is what makes "how did it end"
    // answerable at all.
    assert!(fx.dir(&id).join("session.json").exists());
}

#[test]
fn a_verb_on_a_closed_session_is_refused_with_its_own_exit_code() {
    let Some(fx) = Fixture::new() else {
        return skip("no h5i-browser-light to drive");
    };
    let _ = fx.open(&[]);
    assert!(fx.run(&["browser", "close"]).status.success());

    let out = fx.run(&["browser", "snapshot"]);
    assert_eq!(
        out.status.code(),
        Some(EXIT_SESSION_GONE),
        "a retry loop that cannot tell 'gone' from 'failed' silently starts a second browser"
    );
    let why = String::from_utf8_lossy(&out.stderr);
    assert!(why.contains("was closed"), "{why}");
    assert!(why.contains("--restore"), "the refusal names the way forward");
}

#[test]
fn killing_the_engine_is_recorded_as_a_death_not_papered_over() {
    let Some(fx) = Fixture::new() else {
        return skip("no h5i-browser-light to drive");
    };
    let id = fx.open(&[]);
    let record: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(fx.dir(&id).join("session.json")).unwrap())
            .unwrap();
    let pid = record["control"]["pid"].as_u64().expect("a pid") as i32;

    unsafe { libc::kill(pid, libc::SIGKILL) };
    std::thread::sleep(std::time::Duration::from_millis(300));

    let out = fx.run(&["browser", "snapshot"]);
    assert_eq!(out.status.code(), Some(EXIT_SESSION_GONE));

    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(fx.dir(&id).join("session.json")).unwrap())
            .unwrap();
    assert_eq!(after["state"], "died");
    assert!(after["ended_at"].is_string(), "an ending needs a time");
}

#[test]
fn a_restore_is_a_new_session_with_the_inheritance_written_down() {
    let Some(fx) = Fixture::new() else {
        return skip("no h5i-browser-light to drive");
    };
    let first = fx.open(&[]);
    assert!(fx.run(&["browser", "close"]).status.success());

    let url = fx.site.base.clone();
    let out = fx.run(&["browser", "open", &url, "--allow", "127.0.0.1", "--restore", &first, "--json"]);
    assert!(out.status.success());
    let record: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();

    assert_ne!(record["id"].as_str().unwrap(), first, "ids are not recycled");
    assert_eq!(record["restored_from"].as_str().unwrap(), first);
}

#[test]
fn a_host_session_says_which_lane_its_requests_are() {
    let Some(fx) = Fixture::new() else {
        return skip("no h5i-browser-light to drive");
    };
    let _ = fx.open(&[]);
    let out = fx.run(&["browser", "status"]);
    let text = String::from_utf8_lossy(&out.stdout);
    // The default is honest about being the engine's own account. A page-only
    // claim rendered as host-observed would be the one lie this product cannot
    // afford.
    assert!(text.contains("engine-claimed"), "{text}");
    assert!(text.contains("no containment"), "{text}");
    let _ = fx.run(&["browser", "close"]);
}

#[test]
fn the_control_lock_pauses_a_mutating_verb_and_lets_a_read_through() {
    let Some(fx) = Fixture::new() else {
        return skip("no h5i-browser-light to drive");
    };
    let _ = fx.open(&[]);
    assert!(fx.run(&["browser", "take"]).status.success());

    let click = fx.run(&["browser", "click", "@e1"]);
    assert!(!click.status.success());
    assert!(
        String::from_utf8_lossy(&click.stderr).contains("held by a human"),
        "a mutating verb waits"
    );
    // Watching never collides.
    assert!(fx.run(&["browser", "snapshot"]).status.success());

    assert!(fx.run(&["browser", "release"]).status.success());
    let stale = fx.run(&["browser", "click", "@e1"]);
    assert!(
        String::from_utf8_lossy(&stale.stderr).contains("stale"),
        "the page moved while the human drove"
    );
    let _ = fx.run(&["browser", "close"]);
}

#[test]
fn list_keeps_endings_and_hides_them_by_default() {
    let Some(fx) = Fixture::new() else {
        return skip("no h5i-browser-light to drive");
    };
    let id = fx.open(&[]);
    assert!(fx.run(&["browser", "close"]).status.success());

    let live = fx.run(&["browser", "list", "--json"]);
    let live: Vec<serde_json::Value> = serde_json::from_slice(&live.stdout).unwrap();
    assert!(live.iter().all(|s| s["id"] != id.as_str()));

    let all = fx.run(&["browser", "list", "--all", "--json"]);
    let all: Vec<serde_json::Value> = serde_json::from_slice(&all.stdout).unwrap();
    assert!(all.iter().any(|s| s["id"] == id.as_str()));
}

#[test]
fn an_expired_session_is_an_ending_on_the_record() {
    let Some(fx) = Fixture::new() else {
        return skip("no h5i-browser-light to drive");
    };
    // One second, then a verb after it has passed: the sweep runs on the next
    // command rather than from a timer nothing is holding.
    let id = fx.open(&["--expires-in", "1"]);
    std::thread::sleep(std::time::Duration::from_millis(1500));

    let out = fx.run(&["browser", "snapshot"]);
    assert_eq!(out.status.code(), Some(EXIT_SESSION_GONE));
    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(fx.dir(&id).join("session.json")).unwrap())
            .unwrap();
    assert_eq!(after["state"], "expired");
}

/// The engine writes its request log where the session directory says, and the
/// log is the session's own record — not something the caller assembles.
#[test]
fn the_request_log_lands_in_the_sessions_own_directory() {
    let Some(fx) = Fixture::new() else {
        return skip("no h5i-browser-light to drive");
    };
    let _ = fx.open(&[]);
    let out = fx.run(&["browser", "requests", "--json"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let answer: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(answer.get("requests").is_some(), "{answer}");
    let _ = fx.run(&["browser", "close"]);
}

/// The whole record of a session, in one ordered timeline: what the agent
/// asked for, what the engine decided, who was driving, and how it ended.
#[test]
fn the_audit_carries_the_whole_session_in_one_timeline() {
    let Some(fx) = Fixture::new() else {
        return skip("no h5i-browser-light to drive");
    };
    fx.open(&[]);
    assert!(fx.run(&["browser", "snapshot"]).status.success());
    assert!(fx.run(&["browser", "take"]).status.success());
    assert!(fx.run(&["browser", "release"]).status.success());
    assert!(fx.run(&["browser", "snapshot"]).status.success());
    assert!(fx.run(&["browser", "close"]).status.success());

    let out = fx.run(&["browser", "audit", "--json"]);
    assert!(
        out.status.success(),
        "audit failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let audit: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let events = audit["events"].as_array().expect("a timeline");

    let kinds: Vec<&str> = events
        .iter()
        .filter_map(|e| e["kind"].as_str())
        .collect();
    for expected in ["lifecycle", "agent-action", "request", "control"] {
        assert!(kinds.contains(&expected), "no {expected} row: {kinds:?}");
    }

    // The handover sits between the two snapshots. That ordering is the whole
    // reason the audit exists: "was a human driving when that happened" cannot
    // be answered by a current-holder field.
    let positions: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, e)| e["kind"] == "agent-action" && e["action"].as_str().is_some_and(|a| a.starts_with("snapshot")))
        .map(|(i, _)| i)
        .collect();
    let control = events
        .iter()
        .position(|e| e["kind"] == "control")
        .expect("a handover");
    assert!(
        positions.len() >= 2 && positions[0] < control && control < positions[1],
        "the handover is not between the two snapshots: {kinds:?}"
    );

    // The lanes stay apart: a row h5i wrote from outside is never presented as
    // the engine reporting on itself.
    for event in events {
        let expected = match event["kind"].as_str() {
            Some("lifecycle") | Some("control") => "host-observed",
            _ => "box-claimed",
        };
        assert_eq!(event["lane"], expected, "wrong lane: {event}");
    }
}

/// An audit must say what it could not read. An empty timeline over a log h5i
/// cannot see looks exactly like a session that did nothing.
#[test]
fn the_audit_reports_a_log_it_could_not_read() {
    let Some(fx) = Fixture::new() else {
        return skip("no h5i-browser-light to drive");
    };
    let id = fx.open(&[]);
    assert!(fx.run(&["browser", "close"]).status.success());

    // Take the engine's own logs away, the way an image-backed box would.
    let dir = fx.dir(&id);
    std::fs::remove_file(dir.join("actions.jsonl")).ok();
    std::fs::remove_file(dir.join("requests.jsonl")).ok();

    let out = fx.run(&["browser", "audit", "--json"]);
    let audit: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(audit["sources"]["actions"], "unavailable");
    assert_eq!(audit["sources"]["requests"], "unavailable");

    let text = String::from_utf8_lossy(&fx.run(&["browser", "audit"]).stdout).to_string();
    assert!(text.contains("unavailable"), "{text}");
}

/// The verb that reads the request log does not appear as the cause of it.
#[test]
fn reading_the_log_is_not_recorded_as_causing_it() {
    let Some(fx) = Fixture::new() else {
        return skip("no h5i-browser-light to drive");
    };
    let id = fx.open(&[]);
    assert!(fx.run(&["browser", "requests"]).status.success());
    assert!(fx.run(&["browser", "close"]).status.success());

    let actions = std::fs::read_to_string(fx.dir(&id).join("actions.jsonl")).unwrap();
    let row = actions
        .lines()
        .find(|l| l.contains("\"verb\":\"requests\"") && l.contains("\"phase\":\"result\""))
        .expect("the verb was recorded");
    assert!(
        !row.contains("\"requests\":["),
        "the reader claimed to have caused what it read:\n{row}"
    );
}

/// A page with a hostile heading cannot repaint the terminal it is printed
/// into. The engine carried the bytes; h5i is the last thing between them and a
/// person's screen.
#[test]
fn page_text_reaches_the_terminal_without_its_escape_sequences() {
    let Some(fx) = Fixture::new() else {
        return skip("no h5i-browser-light to drive");
    };
    let url = format!("{}/hostile", fx.site.base);
    let opened = fx.run(&["browser", "open", &url, "--allow", "127.0.0.1", "--json"]);
    assert!(
        opened.status.success(),
        "open failed: {}",
        String::from_utf8_lossy(&opened.stderr)
    );
    let snapshot = fx.run(&["browser", "snapshot"]);
    let text = String::from_utf8_lossy(&snapshot.stdout);
    assert!(!text.contains('\u{1b}'), "an escape reached the terminal");
    let _ = fx.run(&["browser", "close"]);
}
