//! `h5i browser` end to end, against the real binary and a real capture proxy.
//!
//! The browser is faked: a stub `agent-browser` on `PATH` records its argv, and
//! the tests send requests through the proxy themselves. Skipped when the
//! binary has not been built.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Exit status for a verb sent to a session that is not live. Copied rather
/// than imported so a change to the constant has to be made in two places, one
/// of which is a test that says why it matters.
const EXIT_SESSION_GONE: i32 = 69;

fn h5i() -> PathBuf {
    let mut here = std::env::current_exe().expect("test binary path");
    here.pop();
    here.pop();
    here.join("h5i")
}

/// A small HTTP server, so requests travel the path the product is for.
struct Site {
    base: String,
}

impl Site {
    /// Serve until the process ends, one thread per connection.
    fn start() -> Option<Site> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").ok()?;
        let port = listener.local_addr().ok()?.port();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                std::thread::spawn(move || {
                    let _ = Site::serve_one(stream);
                });
            }
        });
        Some(Site {
            base: format!("http://127.0.0.1:{port}"),
        })
    }

    fn serve_one(mut stream: std::net::TcpStream) -> std::io::Result<()> {
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut line = String::new();
        reader.read_line(&mut line)?;
        let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
        let mut cookie = String::new();
        loop {
            let mut header = String::new();
            if reader.read_line(&mut header)? == 0 || header.trim().is_empty() {
                break;
            }
            if let Some(value) = header.strip_prefix("cookie: ").or_else(|| header.strip_prefix("Cookie: ")) {
                cookie = value.trim().to_string();
            }
        }
        let (body, extra) = match path.as_str() {
            "/set" => ("logged in".to_string(), "Set-Cookie: sid=carried; Path=/\r\n"),
            // Echoes what the request carried, so a resend can be told apart
            // from the request it bent.
            p if p.starts_with("/echo") => (format!("path={p} cookie={cookie}"), ""),
            // A response header carrying escape sequences: a server repainting
            // the terminal its answer is printed into.
            "/hostile" => ("x".to_string(), "X-Note: start\u{1b}[2K\u{1b}[1Aoverwritten\r\n"),
            _ => ("<html><body><h1>hello</h1></body></html>".to_string(), ""),
        };
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n{}Connection: close\r\n\r\n{}",
            body.len(),
            extra,
            body
        )?;
        stream.flush()
    }
}

/// A stand-in for agent-browser: records every argv, one line each, and answers
/// the verbs h5i sends in their shape. `open` fetches the URL through the proxy
/// it was handed, which is what the real one does.
const FAKE_AGENT_BROWSER: &str = r#"#!/bin/sh
printf '%s\n' "$*" >> "$H5I_TEST_AB_LOG"
if [ "$1" = "--version" ]; then echo "agent-browser 0.27.0"; exit 0; fi
proxy=""
while [ $# -gt 0 ]; do
  case "$1" in
    --session|--args) shift 2 ;;
    --proxy) proxy="$2"; shift 2 ;;
    --ignore-https-errors) shift ;;
    *) break ;;
  esac
done
verb="$1"; shift
case "$verb" in
  open)
    if [ -n "$proxy" ] && command -v curl >/dev/null 2>&1; then
      curl -s -o /dev/null --proxy "$proxy" "$1" || exit 1
    fi
    echo "✓ opened $1" ;;
  snapshot) printf -- '- heading "hello\033[2K" [ref=e1]\n' ;;
  fill) echo "✓ filled $1 with $2" ;;
  fail) echo "boom" >&2; exit 3 ;;
  *) echo "✓ $verb $*" ;;
esac
"#;

struct Fixture {
    home: tempfile::TempDir,
    site: Site,
    bin: PathBuf,
    ab_log: PathBuf,
}

impl Fixture {
    fn new() -> Option<Fixture> {
        if !h5i().exists() {
            return None;
        }
        let home = tempfile::tempdir().ok()?;
        let site = Site::start()?;
        let bin = home.path().join("bin");
        std::fs::create_dir_all(&bin).ok()?;
        let fake = bin.join("agent-browser");
        std::fs::write(&fake, FAKE_AGENT_BROWSER).ok()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).ok()?;
        }
        let ab_log = home.path().join("agent-browser.log");
        Some(Fixture {
            home,
            site,
            bin,
            ab_log,
        })
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        self.run_with(args, &[])
    }

    fn run_with(&self, args: &[&str], env: &[(&str, &str)]) -> std::process::Output {
        let path = format!(
            "{}:{}",
            self.bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut command = Command::new(h5i());
        command
            .args(args)
            .env("H5I_BROWSER_HOME", self.home.path())
            .env("H5I_TEST_AB_LOG", &self.ab_log)
            .env("PATH", path)
            .env_remove("AGENT_BROWSER_ARGS")
            .env_remove("H5I_BROWSER_SESSION");
        for (name, value) in env {
            command.env(name, value);
        }
        command.output().expect("h5i runs")
    }

    /// A proxy session with no browser, which needs nothing on the host.
    fn proxy(&self, extra: &[&str]) -> String {
        let url = self.site.base.clone();
        let mut args = vec!["browser", "proxy", url.as_str(), "--no-browser", "--json"];
        args.extend_from_slice(extra);
        let out = self.run(&args);
        assert!(
            out.status.success(),
            "proxy failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let record: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("proxy prints a record");
        record["id"].as_str().expect("an id").to_string()
    }

    fn record(&self, id: &str) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(self.dir(id).join("session.json")).unwrap())
            .unwrap()
    }

    fn proxy_url(&self, id: &str) -> String {
        self.record(id)["proxy"]["url"].as_str().unwrap().to_string()
    }

    /// One GET through the session's proxy, as a browser pointed at it sends.
    fn fetch(&self, id: &str, url: &str) -> String {
        let proxy = self.proxy_url(id);
        let authority = proxy.trim_start_matches("http://").trim_end_matches('/');
        let mut stream = std::net::TcpStream::connect(authority).expect("proxy reachable");
        let host = url
            .trim_start_matches("http://")
            .split('/')
            .next()
            .unwrap()
            .to_string();
        write!(
            stream,
            "GET {url} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut answer = String::new();
        stream.read_to_string(&mut answer).unwrap();
        answer
    }

    fn dir(&self, id: &str) -> PathBuf {
        self.home.path().join("sessions").join(id)
    }

    fn ab_calls(&self) -> Vec<String> {
        std::fs::read_to_string(&self.ab_log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn requests(&self) -> serde_json::Value {
        let out = self.run(&["browser", "requests", "--json"]);
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        serde_json::from_slice(&out.stdout).unwrap()
    }
}

fn skip(why: &str) {
    eprintln!("skipping: {why}");
}

fn curl_present() -> bool {
    Command::new("curl").arg("--version").output().is_ok_and(|o| o.status.success())
}

/// The shape an agent actually types. No id anywhere.
#[test]
fn the_ordinary_case_names_no_session() {
    let Some(fx) = Fixture::new() else {
        return skip("no h5i binary to drive");
    };
    let url = fx.site.base.clone();
    let opened = fx.run(&["browser", "open", &url]);
    assert!(opened.status.success(), "{}", String::from_utf8_lossy(&opened.stderr));

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

/// The page verbs reach the session's own Chromium, pointed at its proxy, and
/// say to agent-browser what h5i's verbs mean.
#[test]
fn page_verbs_drive_the_sessions_chromium_through_its_proxy() {
    let Some(fx) = Fixture::new() else {
        return skip("no h5i binary to drive");
    };
    let url = fx.site.base.clone();
    let out = fx.run(&["browser", "open", &url, "--json"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let record: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let id = record["id"].as_str().unwrap().to_string();
    let proxy = record["proxy"]["url"].as_str().unwrap().to_string();

    for args in [
        vec!["browser", "click", "@e1"],
        vec!["browser", "type", "#q", "needle"],
        vec!["browser", "press", "Enter"],
        vec!["browser", "select", "@e2", "blue"],
        vec!["browser", "set-checked", "@e3", "false"],
        vec!["browser", "scroll", "--", "-300"],
        vec!["browser", "wait-for", "--text", "done"],
        vec!["browser", "eval", "document.title"],
        vec!["browser", "reload"],
    ] {
        let out = fx.run(&args);
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    let calls = fx.ab_calls();
    let session_flag = format!("--session h5i-{id} --proxy {proxy} --ignore-https-errors");
    let page_calls: Vec<&String> = calls.iter().filter(|c| !c.starts_with("--version")).collect();
    assert!(
        page_calls.iter().all(|c| c.starts_with(&session_flag)),
        "every call is the session's browser, proxied: {page_calls:#?}"
    );
    for expected in [
        format!("open {url}"),
        "click @e1".into(),
        "fill #q needle".into(),
        "press Enter".into(),
        "select @e2 blue".into(),
        "uncheck @e3".into(),
        "scroll up 300".into(),
        "wait --text done".into(),
        "eval document.title".into(),
        "reload".into(),
    ] {
        assert!(
            page_calls.iter().any(|c| c.ends_with(&expected)),
            "no `{expected}` in {page_calls:#?}"
        );
    }

    // Each is in the action log, beside the proxy's own rows.
    let actions = std::fs::read_to_string(fx.dir(&id).join("actions.jsonl")).unwrap();
    for verb in ["navigate", "click", "type", "eval"] {
        assert!(
            actions.contains(&format!("\"verb\":\"{verb}\"")),
            "no {verb} row in:\n{actions}"
        );
    }
    let _ = fx.run(&["browser", "close"]);
}

/// A browser that is not there leaves no half-made session behind.
#[test]
fn open_without_agent_browser_starts_nothing() {
    let Some(fx) = Fixture::new() else {
        return skip("no h5i binary to drive");
    };
    std::fs::remove_file(fx.bin.join("agent-browser")).unwrap();
    let url = fx.site.base.clone();
    // Only the stub's directory and the system's own, so a real agent-browser
    // on the developer's PATH does not make this pass.
    let out = fx.run_with(
        &["browser", "open", &url],
        &[("PATH", &format!("{}:/usr/bin:/bin", fx.bin.display()))],
    );
    if out.status.success() {
        return skip("an agent-browser is installed system-wide");
    }
    let why = String::from_utf8_lossy(&out.stderr);
    assert!(why.contains("agent-browser"), "{why}");
    assert!(why.contains("--no-browser"), "the refusal names the way forward: {why}");
    let listed = fx.run(&["browser", "list", "--all", "--json"]);
    let listed: Vec<serde_json::Value> = serde_json::from_slice(&listed.stdout).unwrap();
    assert!(listed.is_empty(), "a failed start left a session: {listed:?}");
}

/// A second `open` moves the session it finds. Forking silently would leave the
/// first one holding a page nothing points at.
#[test]
fn opening_again_navigates_rather_than_forking() {
    let Some(fx) = Fixture::new() else {
        return skip("no h5i binary to drive");
    };
    let url = fx.site.base.clone();
    assert!(fx.run(&["browser", "open", &url]).status.success());
    assert!(fx.run(&["browser", "open", &format!("{url}/two")]).status.success());

    let listed = fx.run(&["browser", "list", "--json"]);
    let listed: Vec<serde_json::Value> = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(listed.len(), 1, "a second open forked the session");

    // `--new` is how you say you meant a second one.
    assert!(fx.run(&["browser", "open", &url, "--new"]).status.success());
    let listed = fx.run(&["browser", "list", "--json"]);
    let listed: Vec<serde_json::Value> = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(listed.len(), 2);
    let _ = fx.run(&["browser", "close", "--all"]);
}

/// A session's policy is fixed when its proxy starts, so a flag that would
/// widen it is refused rather than accepted and ignored.
#[test]
fn a_creation_flag_on_a_live_session_is_refused() {
    let Some(fx) = Fixture::new() else {
        return skip("no h5i binary to drive");
    };
    fx.proxy(&[]);
    let url = fx.site.base.clone();
    let out = fx.run(&["browser", "open", &url, "--allow", "example.com"]);
    assert!(!out.status.success());
    let why = String::from_utf8_lossy(&out.stderr);
    assert!(why.contains("--allow"), "{why}");
    assert!(why.contains("--new"), "the refusal names the way forward: {why}");
    let _ = fx.run(&["browser", "close"]);
}

/// Names are for running several at once, and the id still addresses one.
#[test]
fn a_name_addresses_a_session_and_so_does_its_id() {
    let Some(fx) = Fixture::new() else {
        return skip("no h5i binary to drive");
    };
    let auth = fx.proxy(&["--session", "auth"]);
    let public = fx.proxy(&["--session", "public"]);
    assert_ne!(auth, public);

    assert!(fx.run(&["browser", "status", "--session", "auth"]).status.success());
    let by_id = fx.run(&["browser", "status", "--session", &auth, "--json"]);
    assert!(by_id.status.success());
    let record: serde_json::Value = serde_json::from_slice(&by_id.stdout).unwrap();
    assert_eq!(record["id"].as_str().unwrap(), auth);
    assert_eq!(record["name"].as_str().unwrap(), "auth");
    let _ = fx.run(&["browser", "close", "--all"]);
}

/// What the browser sent is in the log, under an id, with the message kept.
#[test]
fn what_went_through_the_proxy_is_logged_and_stored() {
    let Some(fx) = Fixture::new() else {
        return skip("no h5i binary to drive");
    };
    let id = fx.proxy(&[]);
    let answer = fx.fetch(&id, &format!("{}/echo/one", fx.site.base));
    assert!(answer.contains("path=/echo/one"), "{answer}");

    let log = fx.requests();
    assert_eq!(log["ok"], true, "{log}");
    let rows = log["requests"].as_array().unwrap();
    let request = rows
        .iter()
        .find(|r| r["phase"] == "request" && r["url"].as_str().unwrap().ends_with("/echo/one"))
        .unwrap_or_else(|| panic!("the request is not in the log: {log}"));
    assert_eq!(request["allowed"], true);
    assert_eq!(request["addressable"], true);
    let seq = request["seq"].as_u64().unwrap();
    assert!(fx.dir(&id).join("messages").is_dir(), "messages are stored");

    let status = fx.run(&["browser", "status"]);
    let text = String::from_utf8_lossy(&status.stdout);
    assert!(text.contains("proxy-observed"), "{text}");
    assert!(text.contains("messages"), "the store is reported: {text}");
    let _ = seq;
    let _ = fx.run(&["browser", "close"]);
}

/// An origin nobody granted is refused at the proxy, and the refusal is a row.
#[test]
fn an_ungranted_origin_is_refused_and_recorded() {
    let Some(fx) = Fixture::new() else {
        return skip("no h5i binary to drive");
    };
    let id = fx.proxy(&[]);
    let answer = fx.fetch(&id, "http://elsewhere.invalid/secret");
    assert!(answer.starts_with("HTTP/1.1 403"), "{answer}");

    let log = fx.requests();
    assert!(log["denied"].as_u64().unwrap() >= 1, "{log}");
    let denied = fx.run(&["browser", "requests", "--denied-only", "--json"]);
    let denied: serde_json::Value = serde_json::from_slice(&denied.stdout).unwrap();
    assert!(
        denied["requests"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["allowed"] == false),
        "{denied}"
    );
    let _ = fx.run(&["browser", "close"]);
}

/// The workbench verb: one stored request, bent and sent again by h5i, through
/// the same policy and the same jar.
#[test]
fn resend_bends_a_captured_request_and_the_jar_carries_the_login() {
    let Some(fx) = Fixture::new() else {
        return skip("no h5i binary to drive");
    };
    let id = fx.proxy(&[]);
    fx.fetch(&id, &format!("{}/set", fx.site.base));
    fx.fetch(&id, &format!("{}/echo/a?id=1", fx.site.base));
    let log = fx.requests();
    let seq = log["requests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["phase"] == "request" && r["url"].as_str().unwrap().contains("/echo/a"))
        .and_then(|r| r["seq"].as_u64())
        .expect("the request was logged");

    let out = fx.run(&[
        "browser",
        "resend",
        &seq.to_string(),
        "--set",
        "query.id=2",
        "--json",
    ]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let answer: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(answer["ok"], true, "{answer}");
    assert_eq!(answer["response"]["status"], 200);
    assert!(answer["request_id"].as_str().unwrap().starts_with("req_"));

    // A replay is a request like any other: logged, under its own initiator.
    let replays = fx.run(&["browser", "requests", "--initiator", "replay", "--json"]);
    let replays: serde_json::Value = serde_json::from_slice(&replays.stdout).unwrap();
    assert!(
        replays["requests"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["url"].as_str().unwrap().contains("id=2")),
        "{replays}"
    );

    // And through `rpc`, the way the plugins speak it.
    let mut child = Command::new(h5i())
        .args(["browser", "rpc", "--stdio"])
        .env("H5I_BROWSER_HOME", fx.home.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(
        child.stdin.as_mut().unwrap(),
        "{}\n{}",
        serde_json::json!({"id": 1, "verb": "ping"}),
        serde_json::json!({"id": 2, "verb": "resend", "from": seq, "set": ["query.id=3"]})
    )
    .unwrap();
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    let replies: Vec<serde_json::Value> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(replies.len(), 2, "{replies:?}");
    assert_eq!(replies[0]["ok"], true);
    assert_eq!(replies[1]["id"], 2);
    assert_eq!(replies[1]["response"]["status"], 200, "{:?}", replies[1]);
    let _ = fx.run(&["browser", "close"]);
}

/// A verb the session refused must not exit 0. A script that checks the status
/// code would otherwise read "denied by policy" as success.
#[test]
fn a_refused_verb_exits_non_zero() {
    let Some(fx) = Fixture::new() else {
        return skip("no h5i binary to drive");
    };
    fx.proxy(&[]);
    let out = fx.run(&["browser", "resend", "999"]);
    assert!(!out.status.success(), "a resend of nothing exited 0");
    let out = fx.run(&["browser", "socket", "https://not-a-socket.invalid/"]);
    assert!(!out.status.success(), "a socket on an http url exited 0");
    let _ = fx.run(&["browser", "close"]);
}

#[test]
fn a_verb_on_a_closed_session_is_refused_with_its_own_exit_code() {
    let Some(fx) = Fixture::new() else {
        return skip("no h5i binary to drive");
    };
    let id = fx.proxy(&[]);
    assert!(fx.run(&["browser", "close"]).status.success());
    // The record outlives the session: that is what makes "how did it end"
    // answerable at all.
    assert!(fx.dir(&id).join("session.json").exists());

    let out = fx.run(&["browser", "requests"]);
    assert_eq!(
        out.status.code(),
        Some(EXIT_SESSION_GONE),
        "a retry loop that cannot tell 'gone' from 'failed' silently starts a second browser"
    );
    let why = String::from_utf8_lossy(&out.stderr);
    assert!(why.contains("was closed"), "{why}");
}

#[test]
fn killing_the_proxy_is_recorded_as_a_death_not_papered_over() {
    let Some(fx) = Fixture::new() else {
        return skip("no h5i binary to drive");
    };
    let id = fx.proxy(&[]);
    let pid = fx.record(&id)["control"]["pid"].as_u64().expect("a pid") as i32;

    unsafe { libc::kill(pid, libc::SIGKILL) };
    std::thread::sleep(std::time::Duration::from_millis(300));

    let out = fx.run(&["browser", "requests"]);
    assert_eq!(out.status.code(), Some(EXIT_SESSION_GONE));

    let after = fx.record(&id);
    assert_eq!(after["state"], "died");
    assert!(after["ended_at"].is_string(), "an ending needs a time");
}

#[test]
fn the_control_lock_pauses_a_mutating_verb_and_lets_a_read_through() {
    let Some(fx) = Fixture::new() else {
        return skip("no h5i binary to drive");
    };
    let url = fx.site.base.clone();
    assert!(fx.run(&["browser", "open", &url]).status.success());
    assert!(fx.run(&["browser", "take"]).status.success());

    let click = fx.run(&["browser", "click", "@e1"]);
    assert!(!click.status.success());
    assert!(
        String::from_utf8_lossy(&click.stderr).contains("held by a human"),
        "a mutating verb waits"
    );
    // Watching never collides.
    assert!(fx.run(&["browser", "snapshot"]).status.success());
    assert!(fx.run(&["browser", "requests"]).status.success());

    assert!(fx.run(&["browser", "release"]).status.success());
    let stale = fx.run(&["browser", "click", "@e1"]);
    assert!(
        String::from_utf8_lossy(&stale.stderr).contains("stale"),
        "the page moved while the human drove"
    );
    // A fresh reading is what lets the agent act again.
    assert!(fx.run(&["browser", "snapshot"]).status.success());
    assert!(fx.run(&["browser", "click", "@e1"]).status.success(), "handed back, it drives again");
    let _ = fx.run(&["browser", "close"]);
}

#[test]
fn list_keeps_endings_and_hides_them_by_default() {
    let Some(fx) = Fixture::new() else {
        return skip("no h5i binary to drive");
    };
    let id = fx.proxy(&[]);
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
        return skip("no h5i binary to drive");
    };
    // One second, then a verb after it has passed: the sweep runs on the next
    // command rather than from a timer nothing is holding.
    let id = fx.proxy(&["--expires-in", "1"]);
    std::thread::sleep(std::time::Duration::from_millis(1500));

    let pid = fx.record(&id)["control"]["pid"].as_u64().expect("a pid") as i32;
    let out = fx.run(&["browser", "requests"]);
    assert_eq!(out.status.code(), Some(EXIT_SESSION_GONE));
    assert_eq!(fx.record(&id)["state"], "expired");
    // The ending is not only on the record: the proxy is stopped too.
    std::thread::sleep(std::time::Duration::from_millis(300));
    let alive = std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .is_ok_and(|stat| !stat.split_whitespace().nth(2).is_some_and(|state| state == "Z"));
    assert!(!alive, "an expired session's proxy is still running");
}

/// The whole record of a session, in one ordered timeline: what the agent
/// asked for, what the proxy decided, who was driving, and how it ended.
#[test]
fn the_audit_carries_the_whole_session_in_one_timeline() {
    let Some(fx) = Fixture::new() else {
        return skip("no h5i binary to drive");
    };
    let id = fx.proxy(&[]);
    fx.fetch(&id, &format!("{}/echo/x", fx.site.base));
    assert!(fx.run(&["browser", "requests"]).status.success());
    assert!(fx.run(&["browser", "take"]).status.success());
    assert!(fx.run(&["browser", "release"]).status.success());
    assert!(fx.run(&["browser", "requests"]).status.success());
    assert!(fx.run(&["browser", "close"]).status.success());

    let out = fx.run(&["browser", "audit", "--json"]);
    assert!(out.status.success(), "audit failed: {}", String::from_utf8_lossy(&out.stderr));
    let audit: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let events = audit["events"].as_array().expect("a timeline");
    let kinds: Vec<&str> = events.iter().filter_map(|e| e["kind"].as_str()).collect();
    for expected in ["lifecycle", "agent-action", "request", "control"] {
        assert!(kinds.contains(&expected), "no {expected} row: {kinds:?}");
    }

    // The handover sits between the two reads.
    let positions: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            e["kind"] == "agent-action"
                && e["action"].as_str().is_some_and(|a| a.starts_with("requests"))
        })
        .map(|(i, _)| i)
        .collect();
    let control = events
        .iter()
        .position(|e| e["kind"] == "control")
        .expect("a handover");
    assert!(
        positions.len() >= 2 && positions[0] < control && control < positions[1],
        "the handover is not between the two reads: {kinds:?}"
    );
}

/// An audit must say what it could not read. An empty timeline over a log h5i
/// cannot see looks exactly like a session that did nothing.
#[test]
fn the_audit_reports_a_log_it_could_not_read() {
    let Some(fx) = Fixture::new() else {
        return skip("no h5i binary to drive");
    };
    let id = fx.proxy(&[]);
    assert!(fx.run(&["browser", "close"]).status.success());

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
        return skip("no h5i binary to drive");
    };
    let id = fx.proxy(&[]);
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

/// What a page or a server composed cannot repaint the terminal it is printed
/// into. h5i is the last thing between those bytes and a person's screen.
#[test]
fn page_text_reaches_the_terminal_without_its_escape_sequences() {
    let Some(fx) = Fixture::new() else {
        return skip("no h5i binary to drive");
    };
    let url = fx.site.base.clone();
    assert!(fx.run(&["browser", "open", &url]).status.success());
    let snapshot = fx.run(&["browser", "snapshot"]);
    let text = String::from_utf8_lossy(&snapshot.stdout);
    assert!(text.contains("hello"), "{text}");
    assert!(!text.contains('\u{1b}'), "an escape reached the terminal");

    let id = fx.record_of_default();
    fx.fetch(&id, &format!("{}/hostile", fx.site.base));
    let seq = fx.requests()["requests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["url"].as_str().unwrap().ends_with("/hostile"))
        .and_then(|r| r["seq"].as_u64())
        .unwrap();
    let out = fx.run(&["browser", "resend", &seq.to_string()]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(!text.contains('\u{1b}'), "an escape reached the terminal: {text:?}");
    let _ = fx.run(&["browser", "close"]);
}

impl Fixture {
    fn record_of_default(&self) -> String {
        let out = self.run(&["browser", "status", "--json"]);
        let record: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        record["id"].as_str().unwrap().to_string()
    }
}

/// A credential placeholder is resolved on the way into the field, and its
/// value reaches nothing the agent reads: not the reply, not the artifact, not
/// the action log.
#[test]
fn a_typed_credential_reaches_the_browser_and_never_a_reply() {
    let Some(fx) = Fixture::new() else {
        return skip("no h5i binary to drive");
    };
    let url = fx.site.base.clone();
    let env = [("H5I_SECRET_ACME_PASS", "hunter2-from-the-test")];
    assert!(fx.run_with(&["browser", "open", &url], &env).status.success());

    let listed = fx.run_with(&["browser", "env", "--json"], &env);
    let text = String::from_utf8_lossy(&listed.stdout);
    assert!(text.contains("H5I_SECRET_ACME_PASS"), "{text}");
    assert!(!text.contains("hunter2-from-the-test"), "a value was printed: {text}");

    let typed = fx.run_with(&["browser", "type", "#pass", "$H5I_SECRET_ACME_PASS", "--json"], &env);
    assert!(typed.status.success(), "{}", String::from_utf8_lossy(&typed.stderr));
    let reply = String::from_utf8_lossy(&typed.stdout);
    assert!(reply.contains("$H5I_SECRET_ACME_PASS"), "{reply}");
    assert!(!reply.contains("hunter2-from-the-test"), "the value came back: {reply}");

    // The browser got the value; nothing h5i kept did.
    assert!(
        fx.ab_calls().iter().any(|c| c.ends_with("fill #pass hunter2-from-the-test")),
        "{:#?}",
        fx.ab_calls()
    );
    let id = fx.record_of_default();
    for entry in walk(&fx.dir(&id)) {
        let bytes = std::fs::read(&entry).unwrap_or_default();
        assert!(
            !String::from_utf8_lossy(&bytes).contains("hunter2-from-the-test"),
            "the value was kept in {}",
            entry.display()
        );
    }

    // And one that is not set is refused rather than typed as written.
    let out = fx.run(&["browser", "type", "#pass", "$H5I_SECRET_NOT_SET_ANYWHERE"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("H5I_SECRET_NOT_SET_ANYWHERE"));
    let _ = fx.run(&["browser", "close"]);
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}

#[test]
fn a_screenshot_goes_where_the_caller_asked() {
    let Some(fx) = Fixture::new() else {
        return skip("no h5i binary to drive");
    };
    let url = fx.site.base.clone();
    assert!(fx.run(&["browser", "open", &url]).status.success());
    let out_path = fx.home.path().join("shot.png");
    let out = fx.run(&["browser", "screenshot", "--out", out_path.to_str().unwrap(), "--json"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let answer: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(answer["path"], out_path.to_str().unwrap());
    assert!(
        fx.ab_calls()
            .iter()
            .any(|c| c.ends_with(&format!("screenshot {}", out_path.display()))),
        "{:#?}",
        fx.ab_calls()
    );

    // Without `--out`, h5i names the file in the session's artifacts.
    let out = fx.run(&["browser", "screenshot", "--json"]);
    let answer: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(answer["path"].as_str().unwrap().contains("/artifacts/screenshot-"), "{answer}");
    let _ = fx.run(&["browser", "close"]);
}

/// A failing browser verb is a failure, and says what the browser said.
#[test]
fn a_browser_that_fails_is_reported_and_exits_non_zero() {
    let Some(fx) = Fixture::new() else {
        return skip("no h5i binary to drive");
    };
    let url = fx.site.base.clone();
    assert!(fx.run(&["browser", "open", &url]).status.success());
    // `click` with a stub that fails on a verb it does not know: the stub's
    // `fail` verb exits 3, so rewrite the stub for this one call.
    std::fs::write(
        fx.bin.join("agent-browser"),
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 0.27; exit 0; fi\necho boom >&2\nexit 3\n",
    )
    .unwrap();
    let out = fx.run(&["browser", "click", "@e9"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("boom"));
    let _ = fx.run(&["browser", "close"]);
}

/// `open` fetches the page through the proxy, so the very first request of a
/// session is already in its log.
#[test]
fn the_page_open_fetched_is_in_the_log() {
    let Some(fx) = Fixture::new() else {
        return skip("no h5i binary to drive");
    };
    if !curl_present() {
        return skip("no curl for the stub browser to fetch with");
    }
    let url = format!("{}/echo/first", fx.site.base);
    assert!(fx.run(&["browser", "open", &url]).status.success());
    let log = fx.requests();
    assert!(
        log["requests"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["url"].as_str().unwrap().ends_with("/echo/first")),
        "{log}"
    );
    let _ = fx.run(&["browser", "close"]);
}

#[test]
fn removing_a_session_is_refused_while_it_is_live_and_takes_everything_after() {
    let Some(fx) = Fixture::new() else {
        return skip("no h5i binary to drive");
    };
    let id = fx.proxy(&["--session", "doomed"]);
    assert!(fx.dir(&id).join("messages").is_dir(), "the proxy keeps messages");

    let refused = fx.run(&["browser", "rm", "doomed"]);
    assert!(!refused.status.success(), "a live session is not removed by accident");
    assert!(fx.dir(&id).exists());

    fx.run(&["browser", "close", "--session", "doomed"]);
    let removed = fx.run(&["browser", "rm", "doomed"]);
    assert!(removed.status.success(), "rm failed: {}", String::from_utf8_lossy(&removed.stderr));
    assert!(!fx.dir(&id).exists(), "the whole directory goes");
}

#[test]
fn gc_takes_the_stored_bytes_and_leaves_the_account() {
    let Some(fx) = Fixture::new() else {
        return skip("no h5i binary to drive");
    };
    let ended = fx.proxy(&["--session", "ended"]);
    fx.fetch(&ended, &format!("{}/echo/x", fx.site.base));
    fx.run(&["browser", "close", "--session", "ended"]);
    let live = fx.proxy(&["--session", "live"]);

    let out = fx.run(&["browser", "gc", "--older-than", "0"]);
    assert!(out.status.success(), "gc failed: {}", String::from_utf8_lossy(&out.stderr));

    assert!(!fx.dir(&ended).join("messages").exists());
    assert!(fx.dir(&ended).join("capture-reclaimed.json").is_file());
    assert!(fx.dir(&ended).join("session.json").is_file());
    assert!(fx.dir(&ended).join("requests.jsonl").is_file());
    assert!(fx.dir(&live).join("messages").is_dir());
    let _ = fx.run(&["browser", "close", "--all"]);
}

#[test]
fn rm_ended_clears_the_registry_a_close_only_emptied_of_live_sessions() {
    let Some(fx) = Fixture::new() else {
        return skip("no h5i binary to drive");
    };
    let first = fx.proxy(&["--session", "one"]);
    fx.run(&["browser", "close", "--session", "one"]);
    let second = fx.proxy(&["--session", "two"]);

    let closed = fx.run(&["browser", "close", "--all"]);
    assert!(closed.status.success());
    assert!(fx.dir(&first).join("session.json").is_file());
    assert!(fx.dir(&second).join("session.json").is_file());

    let dry = fx.run(&["browser", "rm", "--ended", "--dry-run"]);
    assert!(dry.status.success());
    assert!(fx.dir(&first).exists(), "--dry-run removes nothing");

    let out = fx.run(&["browser", "rm", "--ended"]);
    assert!(out.status.success(), "rm --ended failed: {}", String::from_utf8_lossy(&out.stderr));
    assert!(!fx.dir(&first).exists());
    assert!(!fx.dir(&second).exists());
}

#[test]
fn rm_leaves_a_live_session_alone_when_the_filter_swept_it_up() {
    let Some(fx) = Fixture::new() else {
        return skip("no h5i binary to drive");
    };
    let live = fx.proxy(&["--session", "running"]);

    let out = fx.run(&["browser", "rm", "--all"]);
    assert!(!out.status.success(), "a live session is not removed by a filter");
    assert!(fx.dir(&live).join("session.json").is_file());

    let forced = fx.run(&["browser", "rm", "--all", "--force"]);
    assert!(forced.status.success(), "rm --all --force failed: {}", String::from_utf8_lossy(&forced.stderr));
    assert!(!fx.dir(&live).exists());
}

#[test]
fn close_all_says_what_it_kept_when_nothing_was_live() {
    let Some(fx) = Fixture::new() else {
        return skip("no h5i binary to drive");
    };
    fx.proxy(&["--session", "done"]);
    fx.run(&["browser", "close", "--session", "done"]);

    let out = fx.run(&["browser", "close", "--all"]);
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(said.contains("1 ended session record"), "said: {said}");
    assert!(said.contains("rm --ended"), "said: {said}");
}
