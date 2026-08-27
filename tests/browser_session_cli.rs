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

struct Fixture {
    home: tempfile::TempDir,
    engine: PathBuf,
    page: PathBuf,
}

impl Fixture {
    fn new() -> Option<Fixture> {
        let engine = engine()?;
        let h5i = h5i();
        if !h5i.exists() {
            return None;
        }
        let home = tempfile::tempdir().ok()?;
        let page = home.path().join("page.html");
        std::fs::write(
            &page,
            "<html><head><title>t</title></head><body><h1>hello</h1>\
             <p>a <a href=\"https://example.com/next\">link</a></p></body></html>",
        )
        .ok()?;
        Some(Fixture { home, engine, page })
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        Command::new(h5i())
            .args(args)
            .env("H5I_BROWSER_HOME", self.home.path())
            .env("H5I_BROWSER_ENGINE", &self.engine)
            .output()
            .expect("h5i runs")
    }

    fn start(&self, extra: &[&str]) -> String {
        let url = format!("file://{}", self.page.display());
        let mut args = vec!["browser", "start", url.as_str(), "--json"];
        args.extend_from_slice(extra);
        let out = self.run(&args);
        assert!(
            out.status.success(),
            "start failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let record: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("start prints a record");
        record["id"].as_str().expect("an id").to_string()
    }

    fn dir(&self, id: &str) -> PathBuf {
        self.home.path().join("sessions").join(id)
    }
}

fn skip(why: &str) {
    eprintln!("skipping: {why}");
}

#[test]
fn a_session_starts_answers_and_closes() {
    let Some(fx) = Fixture::new() else {
        return skip("no h5i-browser-light to drive");
    };
    let id = fx.start(&[]);

    let snapshot = fx.run(&["browser", "snapshot", &id]);
    assert!(snapshot.status.success());
    let text = String::from_utf8_lossy(&snapshot.stdout);
    assert!(text.contains("hello"), "{text}");

    // The engine fences page content for a model. h5i must not undo that.
    assert!(text.contains("UNTRUSTED"), "{text}");

    let closed = fx.run(&["browser", "close", &id]);
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
    let id = fx.start(&[]);
    assert!(fx.run(&["browser", "close", &id]).status.success());

    let out = fx.run(&["browser", "snapshot", &id]);
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
    let id = fx.start(&[]);
    let record: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(fx.dir(&id).join("session.json")).unwrap())
            .unwrap();
    let pid = record["control"]["pid"].as_u64().expect("a pid") as i32;

    unsafe { libc::kill(pid, libc::SIGKILL) };
    std::thread::sleep(std::time::Duration::from_millis(300));

    let out = fx.run(&["browser", "snapshot", &id]);
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
    let first = fx.start(&[]);
    assert!(fx.run(&["browser", "close", &first]).status.success());

    let url = format!("file://{}", fx.page.display());
    let out = fx.run(&["browser", "start", &url, "--restore", &first, "--json"]);
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
    let id = fx.start(&[]);
    let out = fx.run(&["browser", "status", &id]);
    let text = String::from_utf8_lossy(&out.stdout);
    // The default is honest about being the engine's own account. A page-only
    // claim rendered as host-observed would be the one lie this product cannot
    // afford.
    assert!(text.contains("engine-claimed"), "{text}");
    assert!(text.contains("no containment"), "{text}");
    let _ = fx.run(&["browser", "close", &id]);
}

#[test]
fn the_control_lock_pauses_a_mutating_verb_and_lets_a_read_through() {
    let Some(fx) = Fixture::new() else {
        return skip("no h5i-browser-light to drive");
    };
    let id = fx.start(&[]);
    assert!(fx.run(&["browser", "take", &id]).status.success());

    let click = fx.run(&["browser", "click", &id, "@e1"]);
    assert!(!click.status.success());
    assert!(
        String::from_utf8_lossy(&click.stderr).contains("held by a human"),
        "a mutating verb waits"
    );
    // Watching never collides.
    assert!(fx.run(&["browser", "snapshot", &id]).status.success());

    assert!(fx.run(&["browser", "release", &id]).status.success());
    let stale = fx.run(&["browser", "click", &id, "@e1"]);
    assert!(
        String::from_utf8_lossy(&stale.stderr).contains("stale"),
        "the page moved while the human drove"
    );
    let _ = fx.run(&["browser", "close", &id]);
}

#[test]
fn list_keeps_endings_and_hides_them_by_default() {
    let Some(fx) = Fixture::new() else {
        return skip("no h5i-browser-light to drive");
    };
    let id = fx.start(&[]);
    assert!(fx.run(&["browser", "close", &id]).status.success());

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
    let id = fx.start(&["--expires-in", "1"]);
    std::thread::sleep(std::time::Duration::from_millis(1500));

    let out = fx.run(&["browser", "snapshot", &id]);
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
    let id = fx.start(&[]);
    let out = fx.run(&["browser", "requests", &id, "--json"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let answer: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(answer.get("requests").is_some(), "{answer}");
    let _ = fx.run(&["browser", "close", &id]);
}

/// A local file with a hostile title cannot repaint the terminal it is printed
/// into. The engine carried the bytes; h5i is the last thing between them and a
/// person's screen.
#[test]
fn page_text_reaches_the_terminal_without_its_escape_sequences() {
    let Some(fx) = Fixture::new() else {
        return skip("no h5i-browser-light to drive");
    };
    let hostile = fx.home.path().join("hostile.html");
    std::fs::write(
        &hostile,
        "<html><body><h1>start\u{1b}[2K\u{1b}[1Aoverwritten</h1></body></html>",
    )
    .unwrap();
    let url = format!("file://{}", hostile.display());
    let out = fx.run(&["browser", "start", &url, "--json"]);
    let id = serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let snapshot = fx.run(&["browser", "snapshot", &id]);
    let text = String::from_utf8_lossy(&snapshot.stdout);
    assert!(!text.contains('\u{1b}'), "an escape reached the terminal");
    let _ = fx.run(&["browser", "close", &id]);
}
