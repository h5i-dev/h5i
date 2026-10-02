//! End-to-end tests for the box console (`h5i ui`).
#![cfg(feature = "web")]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

use tempfile::TempDir;

const H5I: &str = env!("CARGO_BIN_EXE_h5i");

// ─── a repository with boxes in it ───────────────────────────────────────────

struct Repo {
    dir: PathBuf,
    /// Where browser sessions live for this test. Sessions are the machine's,
    /// not the repository's, so a test that did not pin this would read (and
    /// show) the developer's own registry.
    browser_home: PathBuf,
    /// Where projects live for this test, pinned for the same reason as
    /// `browser_home`: the project store is the machine's, not the repo's.
    project_home: PathBuf,
    _root: TempDir,
}

impl Repo {
    fn new() -> Repo {
        let root = TempDir::new().expect("tempdir");
        let dir = root.path().join("repo");
        ok(Command::new("git").args(["init", "-b", "main"]).arg(&dir));
        git(&dir, &["config", "user.name", "Console Tester"]);
        git(&dir, &["config", "user.email", "console@h5i.test"]);
        std::fs::write(dir.join("README.md"), "seed\n").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-m", "seed"]);
        let browser_home = root.path().join("browser-home");
        std::fs::create_dir_all(&browser_home).expect("browser home");
        let project_home = root.path().join("project-home");
        std::fs::create_dir_all(&project_home).expect("project home");
        Repo {
            dir,
            browser_home,
            project_home,
            _root: root,
        }
    }

    /// Run `h5i` and hand back whatever it did.
    fn h5i(&self, args: &[&str]) -> Output {
        Command::new(H5I)
            .args(args)
            .envs(self.env())
            .current_dir(&self.dir)
            .output()
            .expect("failed to run h5i")
    }

    fn h5i_ok(&self, args: &[&str]) -> Output {
        let out = self.h5i(args);
        assert!(
            out.status.success(),
            "h5i {} failed:\nstdout: {}\nstderr: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        out
    }

    /// Hermetic: a fixed agent identity, and the workspace tier pinned so box
    /// creation never probes the host. These tests are about the HTTP surface,
    /// not about confinement. The kernel tiers are `env_integration.rs`'s job.
    fn env(&self) -> [(&'static str, String); 4] {
        [
            ("H5I_AGENT", "tester".to_string()),
            ("H5I_DEFAULT_ISOLATION", "workspace".to_string()),
            (
                "H5I_BROWSER_HOME",
                self.browser_home.display().to_string(),
            ),
            ("H5I_PROJECT_HOME", self.project_home.display().to_string()),
        ]
    }

    /// Write a session record straight into the registry.
    ///
    /// Rather than opening one: these tests are about the HTTP surface, and a
    /// real session would need a target to fetch and an engine to run.
    fn session(&self, id: &str, name: &str, live: bool) -> PathBuf {
        let dir = self.browser_home.join("sessions").join(id);
        std::fs::create_dir_all(&dir).expect("session dir");
        let dir = dir.clone();
        let record = serde_json::json!({
            "id": id,
            "name": name,
            "engine": "h5i-light",
            "lane": "engine-claimed",
            "placement": {"kind": "host"},
            "url": "http://127.0.0.1:1/",
            "started_at": "2026-09-07T10:00:00.000000Z",
            "expires_at": null,
            "storage": "ephemeral",
            "policy_digest": "sha256:test",
            "identity": "native",
            "identity_digest": "test",
            "restored_from": null,
            "state": if live { "live" } else { "closed" },
            "ended_at": if live { serde_json::Value::Null } else { "2026-09-07T10:05:00.000000Z".into() },
            "end_reason": if live { serde_json::Value::Null } else { "closed by the user".into() },
            // A tagged enum, as the record writes it. Getting this shape wrong
            // is silently invisible: `list` skips a record it cannot read.
            "confinement": {"kind": "process"},
            "enclosing_box": null,
            "control": {"channel": "port", "file": null, "witness": null, "pid": null},
            "logs": {
                "actions": null,
                "requests": dir.join("requests.jsonl").display().to_string(),
            },
            "permissive_cors": false,
        });
        std::fs::write(
            dir.join("session.json"),
            serde_json::to_vec(&record).expect("record"),
        )
        .expect("write record");
        dir
    }
}

fn git(dir: &Path, args: &[&str]) {
    ok(Command::new("git").args(args).current_dir(dir));
}

fn ok(cmd: &mut Command) {
    let out = cmd.output().expect("spawn");
    assert!(
        out.status.success(),
        "{:?} failed: {}",
        cmd,
        String::from_utf8_lossy(&out.stderr)
    );
}

// ─── the console under test ──────────────────────────────────────────────────

struct Console {
    child: Child,
    /// `127.0.0.1:<port>`: also the `Host` header and the self-origin.
    addr: String,
    token: String,
}

impl Console {
    /// Start `h5i ui` on an OS-assigned port and wait for its URL line.
    ///
    /// Reading that line is a sufficient handshake, by construction: the
    /// command binds the listener *before* it prints (that is why
    /// `Console::bind` and `Console::serve` are separate), so by the time the
    /// URL exists the socket is listening and a connection can only queue in
    /// the accept backlog. Never be refused.
    fn start(repo: &Repo) -> Console {
        let mut child = Command::new(H5I)
            .args(["ui", "--port", "0"])
            .envs(repo.env())
            .current_dir(&repo.dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn h5i ui");

        let stdout = child.stdout.take().expect("piped stdout");
        let mut lines = BufReader::new(stdout).lines();
        let url = loop {
            match lines.next() {
                Some(Ok(line)) if line.contains("http://127.0.0.1:") => break line,
                Some(Ok(_)) => continue,
                // The process died before printing a URL. Kill-and-report
                // rather than block forever on a pipe that will never fill.
                _ => {
                    let _ = child.kill();
                    let mut err = String::new();
                    if let Some(mut e) = child.stderr.take() {
                        let _ = e.read_to_string(&mut err);
                    }
                    panic!("h5i ui exited before printing a URL; stderr:\n{err}");
                }
            }
        };

        let url = url.trim();
        let start = url.find("http://").expect("a url on the line");
        let url = &url[start..];
        let rest = url.strip_prefix("http://").unwrap();
        let (addr, query) = rest.split_once("/?").expect("host and query");
        let token = query
            .strip_prefix("token=")
            .expect("a token in the query")
            .to_string();
        assert_eq!(token.len(), 32, "128 bits of hex: {token}");

        // Keep draining stdout for the life of the console. Dropping the read
        // end here would close the pipe, and the next line the command prints
        // (it prints two more after the URL) would kill it, which surfaces as
        // a connection reset three assertions later, only when the machine is
        // busy enough for the ordering to flip. Detached: the reader ends when
        // `Drop` kills the child and the pipe closes.
        std::thread::spawn(move || lines.for_each(drop));

        Console {
            child,
            addr: addr.to_string(),
            token,
        }
    }

    fn get(&self, path: &str) -> Reply {
        self.request("GET", path, &[])
    }

    fn get_with(&self, path: &str, headers: &[(&str, &str)]) -> Reply {
        self.request("GET", path, headers)
    }

    /// With the token in the query string, the way the printed URL carries it.
    fn get_authed(&self, path: &str) -> Reply {
        let sep = if path.contains('?') { '&' } else { '?' };
        self.request("GET", &format!("{path}{sep}token={}", self.token), &[])
    }

    /// With the cookie the page holds after its first load.
    fn get_with_cookie(&self, path: &str) -> Reply {
        self.request(
            "GET",
            path,
            &[("Cookie", &format!("h5i_console={}", self.token))],
        )
    }

    fn request(&self, method: &str, path: &str, headers: &[(&str, &str)]) -> Reply {
        let mut sock = TcpStream::connect(&self.addr).expect("connect to the console");
        let mut head = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n",
            self.addr
        );
        for (k, v) in headers {
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        head.push_str("\r\n");
        sock.write_all(head.as_bytes()).expect("write request");
        // `Connection: close` makes the server hang up after the response, so
        // read-to-end terminates without parsing Content-Length.
        let mut raw = Vec::new();
        sock.read_to_end(&mut raw).expect("read response");
        Reply::parse(&raw)
    }

    fn origin(&self) -> String {
        format!("http://{}", self.addr)
    }
}

impl Drop for Console {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Reply {
    status: u16,
    headers: String,
    body: Vec<u8>,
}

impl Reply {
    fn parse(raw: &[u8]) -> Reply {
        let split = raw
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .expect("a header/body boundary");
        let head = String::from_utf8_lossy(&raw[..split]).into_owned();
        let status = head
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|c| c.parse().ok())
            .expect("a status code");
        Reply {
            status,
            headers: head.to_lowercase(),
            body: raw[split + 4..].to_vec(),
        }
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("expected JSON, got {e}: {}", self.text()))
    }
}

// ─── the gate ────────────────────────────────────────────────────────────────

#[test]
fn the_console_is_reachable_only_with_the_token_it_printed() {
    let repo = Repo::new();
    let ui = Console::start(&repo);

    assert_eq!(ui.get("/api/boxes").status, 401, "no token at all");
    assert_eq!(
        ui.get("/api/boxes?token=deadbeefdeadbeefdeadbeefdeadbeef")
            .status,
        401,
        "a wrong token of the right length"
    );
    assert_eq!(
        ui.get_with("/api/boxes", &[("Cookie", "h5i_console=nope")])
            .status,
        401,
        "a wrong cookie"
    );
    // Another loopback service's cookie is not this console's.
    assert_eq!(
        ui.get_with(
            "/api/boxes",
            &[("Cookie", &format!("jupyter={}", ui.token))]
        )
        .status,
        401,
        "the right value under the wrong cookie name"
    );

    assert_eq!(ui.get_authed("/api/boxes").status, 200);
    assert_eq!(ui.get_with_cookie("/api/boxes").status, 200);
}

#[test]
fn a_page_on_another_origin_cannot_read_the_console_even_holding_the_token() {
    let repo = Repo::new();
    let ui = Console::start(&repo);

    let refused = ui.get_with(
        &format!("/api/boxes?token={}", ui.token),
        &[("Origin", "http://evil.test")],
    );
    assert_eq!(refused.status, 403, "{}", refused.text());

    // The page's own fetches send Origin too, and must still get through.
    let same = ui.get_with(
        "/api/boxes",
        &[
            ("Origin", &ui.origin()),
            ("Cookie", &format!("h5i_console={}", ui.token)),
        ],
    );
    assert_eq!(same.status, 200, "{}", same.text());
}

#[test]
fn loading_the_page_hands_over_the_cookie_the_rest_of_the_session_uses() {
    let repo = Repo::new();
    let ui = Console::start(&repo);

    let page = ui.get_authed("/");
    assert_eq!(page.status, 200);
    assert!(
        page.headers.contains(&format!(
            "set-cookie: h5i_console={}; path=/; httponly; samesite=strict",
            ui.token
        )),
        "the page must set a strict, script-invisible cookie; got:\n{}",
        page.headers
    );
}

#[test]
fn the_console_has_no_way_to_change_anything() {
    let repo = Repo::new();
    let ui = Console::start(&repo);

    // Read-only is the console's central claim, and it is only true as long as
    // no route answers a mutating method. 405 means the router knows the path
    // and refuses the verb, which is the assertion, not 404.
    for method in ["POST", "PUT", "PATCH", "DELETE"] {
        let reply = ui.request(
            method,
            &format!("/api/boxes?token={}", ui.token),
            &[("Content-Length", "0")],
        );
        assert_eq!(
            reply.status, 405,
            "{method} /api/boxes should be method-not-allowed, got {}",
            reply.status
        );
    }
}

// ─── the payloads ────────────────────────────────────────────────────────────

#[test]
fn an_empty_repository_renders_an_empty_fleet_rather_than_failing() {
    let repo = Repo::new();
    let ui = Console::start(&repo);

    let boxes = ui.get_authed("/api/boxes");
    assert_eq!(boxes.status, 200);
    assert_eq!(boxes.json().as_array().expect("an array").len(), 0);

    // A host report is available before any box exists. It is the top strip,
    // and a fleet of zero is exactly when someone reads it.
    let probe = ui.get_authed("/api/probe");
    assert_eq!(probe.status, 200);
    let probe = probe.json();
    assert!(probe["os"].is_string(), "{probe}");
    assert!(
        probe["claims"].as_array().is_some_and(|c| !c.is_empty()),
        "the probe must enumerate the isolation claims: {probe}"
    );

    assert_eq!(ui.get_authed("/api/box/tester/nope").status, 404);
}

#[test]
fn a_box_and_its_run_reach_the_fleet_and_the_detail_pane() {
    let repo = Repo::new();
    repo.h5i_ok(&["box", "create", "consoled"]);
    repo.h5i_ok(&["box", "run", "consoled", "--", "sh", "-c", "echo hello"]);
    let ui = Console::start(&repo);

    let fleet = ui.get_authed("/api/boxes").json();
    let rows = fleet.as_array().expect("an array");
    assert_eq!(rows.len(), 1, "{fleet}");
    let row = &rows[0];
    // The manifest is flattened in, so a fleet row is a superset of
    // `h5i box list --json`. If that ever stops being true the console and the
    // scriptable CLI have started describing a box two different ways.
    assert_eq!(row["id"], "env/tester/consoled");
    assert_eq!(row["agent"], "tester");
    assert_eq!(row["isolation_claim"], "workspace");
    assert!(row["policy_digest"].as_str().is_some_and(|d| d.len() == 64));
    assert_eq!(row["drift"], "up-to-date");
    assert_eq!(row["has_workspace"], true);

    let signals = &row["signals"];
    assert_eq!(signals["runs"], 1, "the run should have left a receipt");
    assert_eq!(signals["failed"], 0);
    assert_eq!(signals["egress_denied"], 0);
    assert_eq!(signals["verdict"], "clean");
    // Workspace tier confines nothing, and the console has to keep saying so
    // rather than let a green "clean" imply containment.
    assert_eq!(signals["weak_isolation"], true);
    assert_eq!(signals["host_observed"], 1);
    assert_eq!(signals["box_claimed_only"], false);

    let detail = ui.get_authed("/api/box/tester/consoled");
    assert_eq!(detail.status, 200);
    let detail = detail.json();
    assert_eq!(detail["item"]["id"], "env/tester/consoled");
    assert_eq!(detail["policy"]["isolation"], "workspace");
    assert!(
        detail["policy"]["wall_secs"].as_u64().is_some(),
        "the enforced-policy panel needs the limits: {}",
        detail["policy"]
    );
    assert!(
        detail["events"]
            .as_array()
            .is_some_and(|e| e.iter().any(|ev| ev["event"] == "created")),
        "the event log should carry creation: {}",
        detail["events"]
    );

    let receipts = detail["receipts"].as_array().expect("receipts");
    assert_eq!(receipts.len(), 1, "{:?}", receipts);
    let receipt = &receipts[0];
    assert_eq!(receipt["exit_code"], 0);
    assert_eq!(
        receipt["source"], "host-env-run",
        "an `h5i box run` is observed from the host, not claimed by the box"
    );
    assert_eq!(detail["receipts_folded"], 0);

    // The rendered receipt is the same text `h5i box inspect` prints.
    let id = receipt["id"].as_str().expect("a receipt id");
    let render = ui.get_authed(&format!("/api/box/tester/consoled/receipts/{id}"));
    assert_eq!(render.status, 200);
    let text = render.json()["render"]
        .as_str()
        .expect("a render field")
        .to_string();
    assert!(text.contains("echo hello"), "{text}");
    assert!(text.contains("env/tester/consoled"), "{text}");
}

#[test]
fn one_boxs_receipt_cannot_be_read_through_another_box() {
    let repo = Repo::new();
    repo.h5i_ok(&["box", "create", "alpha"]);
    repo.h5i_ok(&["box", "create", "beta"]);
    repo.h5i_ok(&["box", "run", "alpha", "--", "sh", "-c", "echo secret"]);
    let ui = Console::start(&repo);

    let alpha = ui.get_authed("/api/box/tester/alpha").json();
    let id = alpha["receipts"][0]["id"]
        .as_str()
        .expect("alpha has a receipt")
        .to_string();

    assert_eq!(
        ui.get_authed(&format!("/api/box/tester/alpha/receipts/{id}"))
            .status,
        200,
        "its own box can read it"
    );
    // `env::inspect` enforces ownership; the route must not have a way around
    // it, or a capture id would be a read primitive over every box on the host.
    assert_eq!(
        ui.get_authed(&format!("/api/box/tester/beta/receipts/{id}"))
            .status,
        404,
        "a sibling box must not be able to read it"
    );
}

// ─── the bundle ──────────────────────────────────────────────────────────────

#[test]
fn the_binary_carries_a_real_console_and_not_the_build_scripts_stub() {
    let repo = Repo::new();
    let ui = Console::start(&repo);

    let page = ui.get_authed("/");
    assert_eq!(page.status, 200);
    let html = page.text();

    // The stub `build.rs` writes when H5I_SKIP_WEB_BUILD is set says so in as
    // many words. A binary serving it would look completely healthy (200, a
    // page, no error anywhere) which is exactly why it is asserted against.
    assert!(
        !html.contains("console bundle not built"),
        "this binary embeds the build-script stub, not the console. Build the \
         frontend (`cd web && npm ci && npm run build`) and rebuild without \
         H5I_SKIP_WEB_BUILD.\n{html}"
    );

    let asset = html
        .split_once("/assets/")
        .map(|(_, rest)| rest.split(['"', '\'']).next().unwrap_or("").to_string())
        .expect("the page should reference a bundled asset");
    assert!(asset.ends_with(".js"), "expected a script asset, got {asset}");

    let served = ui.get_authed(&format!("/assets/{asset}"));
    assert_eq!(served.status, 200, "the referenced asset must be embedded");
    assert!(
        served.headers.contains("content-type: text/javascript"),
        "assets need their real media type or the browser refuses the module:\n{}",
        served.headers
    );
    assert!(!served.body.is_empty());

    assert_eq!(
        ui.get_authed("/assets/does-not-exist.js").status,
        404,
        "a missing asset is a 404, not a panic"
    );
}

#[test]
fn the_console_ships_the_same_fence_the_engine_prints() {
    // The engine wraps page content before it reaches a *model*, because that
    // is the moment attacker-controlled text meets something deciding what to do
    // next. The console showed the same text (page URLs, console output, policy
    // subjects, the rendered frame) to a *person*, with no boundary at all,
    // which left the human reader with less framing than the model got.
    //
    // Asserted against the served bundle rather than the source, because a
    // component that exists and is never rendered would pass a source grep and
    // fail the reader.
    let repo = Repo::new();
    let ui = Console::start(&repo);

    let html = ui.get_authed("/").text();
    let mut scripts: Vec<String> = Vec::new();
    let mut rest = html.as_str();
    while let Some((_, after)) = rest.split_once("/assets/") {
        let name = after.split(['"', '\'']).next().unwrap_or("").to_string();
        if name.ends_with(".js") {
            scripts.push(name);
        }
        rest = after;
    }
    assert!(!scripts.is_empty(), "no script assets referenced:\n{html}");

    // The markers are byte-identical to the engine's on purpose: a reader who
    // has seen one should recognise the other.
    let begin = "--- BEGIN UNTRUSTED PAGE CONTENT ---";
    let end = "--- END UNTRUSTED PAGE CONTENT ---";
    let found = scripts.iter().any(|name| {
        let body = ui.get_authed(&format!("/assets/{name}")).body;
        let text = String::from_utf8_lossy(&body);
        text.contains(begin) && text.contains(end)
    });
    assert!(
        found,
        "the console bundle does not carry the fence markers, so the page-derived \
         panes are rendered to a person with no boundary around them"
    );
}

// ─── sessions ────────────────────────────────────────────────────────────────

#[test]
fn the_console_lists_browser_sessions_with_the_evidence_behind_each_state() {
    let repo = Repo::new();
    repo.session("br_live", "watching", true);
    repo.session("br_over", "finished", false);
    let ui = Console::start(&repo);

    let fleet = ui.get_authed("/api/sessions").json();
    assert_eq!(fleet["total"], 2);
    assert_eq!(fleet["live"], 1);

    let rows = fleet["sessions"].as_array().expect("sessions");
    let by_name = |name: &str| {
        rows.iter()
            .find(|r| r["name"] == name)
            .unwrap_or_else(|| panic!("no session named {name} in {rows:?}"))
            .clone()
    };

    // A record that says live with no engine behind it is the one case this
    // cannot classify, and the console says so rather than guessing.
    let live = by_name("watching");
    assert_eq!(live["attention"]["state"], "unknown");
    assert!(
        live["attention"]["why"]
            .as_str()
            .unwrap_or_default()
            .contains("control file"),
        "a state has to carry its evidence: {live:?}"
    );

    // An ending nobody has read is `done`; which client has read it is the
    // client's business, not the server's.
    let over = by_name("finished");
    assert_eq!(over["attention"]["state"], "done");
    assert_eq!(over["attention"]["why"], "closed by the user");
}

#[test]
fn a_session_detail_answers_with_its_own_files_and_nothing_else() {
    let repo = Repo::new();
    let dir = repo.session("br_one", "one", false);
    std::fs::write(
        dir.join("requests.jsonl"),
        "{\"seq\":0,\"at\":\"2026-09-07T10:00:00.000000Z\",\"phase\":\"request\",\
         \"initiator\":\"navigation\",\"method\":\"GET\",\
         \"url\":\"https://target.test/a\",\"allowed\":true}\n         {\"seq\":1,\"at\":\"2026-09-07T10:00:01.000000Z\",\"phase\":\"request\",\
         \"initiator\":\"subresource\",\"method\":\"GET\",\
         \"url\":\"https://elsewhere.test/x\",\"allowed\":false,\
         \"denied_reason\":\"not in the allowlist\"}\n",
    )
    .expect("write log");
    let ui = Console::start(&repo);

    let detail = ui.get_authed("/api/session/br_one").json();
    assert_eq!(detail["requests"], 2, "both fetches counted");
    assert_eq!(detail["denied"], 1, "a refusal is counted apart");
    assert_eq!(detail["captured"], serde_json::Value::Null, "capture was off");
    assert_eq!(
        detail["requests_log"].as_array().map(Vec::len),
        Some(2),
        "the log itself, for the request table"
    );
}

#[test]
fn a_session_id_that_is_not_one_component_reads_nothing() {
    let repo = Repo::new();
    repo.session("br_one", "one", true);
    let ui = Console::start(&repo);

    // The id becomes a path. A boxed session writes its own record, so this is
    // target-adjacent input, and the guard belongs on the route.
    for id in ["..", "%2e%2e%2fetc", "a/b"] {
        let reply = ui.get_authed(&format!("/api/session/{id}"));
        assert_ne!(
            reply.status, 200,
            "`{id}` should not resolve to a session, got {}",
            reply.status
        );
    }
}

// ─── projects ────────────────────────────────────────────────────────────────

#[test]
fn the_console_serves_a_project_its_findings_and_a_rendered_report() {
    let repo = Repo::new();
    // Build a project entirely through the CLI, the way a person would.
    repo.h5i_ok(&["project", "init", "acme", "--title", "ACME web"]);
    repo.h5i_ok(&[
        "project", "evidence", "add-text", "-p", "acme", "GET /admin returned 200", "--caption", "admin open",
    ]);
    repo.h5i_ok(&[
        "project", "finding", "create", "-p", "acme", "--title", "Admin panel is open",
        "--severity", "high", "--summary", "Any account reaches the admin panel.", "--evidence", "E-1",
    ]);
    repo.h5i_ok(&["project", "report", "set", "-p", "acme"]); // draft from stdin (empty) — replace below
    // A report body that exercises directives and a glossary term.
    let body = "# Report\n\n{{findings}}\n\n{{finding F-1}}\n\nThis is an {{term authorization}} gap.\n\n{{glossary}}\n";
    let out = Command::new(H5I)
        .args(["project", "report", "set", "-p", "acme"])
        .envs(repo.env())
        .current_dir(&repo.dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .and_then(|mut c| {
            use std::io::Write as _;
            c.stdin.take().unwrap().write_all(body.as_bytes())?;
            c.wait_with_output()
        })
        .expect("set report");
    assert!(out.status.success(), "set report: {}", String::from_utf8_lossy(&out.stderr));

    let ui = Console::start(&repo);

    // The project appears in the list, with its counts.
    let projects = ui.get_authed("/api/projects").json();
    let rows = projects.as_array().expect("an array");
    let acme = rows.iter().find(|p| p["name"] == "acme").expect("acme in list");
    assert_eq!(acme["findings"], 1);
    assert_eq!(acme["open_findings"], 1);

    // Its detail carries the finding and the evidence copy.
    let detail = ui.get_authed("/api/project/acme").json();
    assert_eq!(detail["findings"][0]["id"], "F-1");
    assert_eq!(detail["findings"][0]["severity"], "high");
    assert_eq!(detail["evidence"][0]["id"], "E-1");
    assert!(detail["has_draft"].as_bool().unwrap_or(false));

    // The report renders: directives resolved, the term marked, the glossary
    // appendix present with its external reference.
    let report = ui.get_authed("/api/project/acme/report").json();
    let html = report["html"].as_str().expect("html");
    assert!(html.contains("findings-table"), "findings table: {html}");
    assert!(html.contains("Admin panel is open"));
    assert!(html.contains("data-term=\"authorization\""), "term marked: {html}");
    assert!(html.contains("id=\"term-authorization\""), "glossary appendix");
    assert!(report["terms"].as_array().map(|t| !t.is_empty()).unwrap_or(false));

    // A project that does not exist is a 404, not a 500.
    assert_eq!(ui.get_authed("/api/project/nope").status, 404);

    // The glossary is served for the definition panel.
    let glossary = ui.get_authed("/api/glossary").json();
    assert!(glossary.as_array().map(|t| t.len() > 20).unwrap_or(false));
}

// ─── apps: the repository's proof projects ────────────────────────────────────

/// A proof project laid out the way `examples/app/*/proofs` are, with a
/// receipt as `cargo app-verify` writes it.
fn write_proof_project(repo: &Repo) {
    let app = repo.dir.join("examples/app/notes");
    std::fs::create_dir_all(app.join("proofs/generated")).unwrap();
    std::fs::create_dir_all(app.join("kernel/src")).unwrap();
    std::fs::create_dir_all(app.join("server/src")).unwrap();
    std::fs::write(app.join("kernel/src/lib.rs"), "pub fn transition() {}\n").unwrap();
    std::fs::write(app.join("server/src/lib.rs"), "// h5i-allow: no-actor (health route)\nfn health() {}\n").unwrap();
    std::fs::write(app.join("server/Cargo.toml"), "[package]\nname = \"notes-server\"\n").unwrap();
    std::fs::write(app.join("proofs/lakefile.lean"), "package notes_proofs\n").unwrap();
    std::fs::write(
        app.join("proofs/generated/NotesKernel.lean"),
        "inductive Role where\n| Reader : Role\n| Writer : Role\n\ninductive Action where\n| Read : Action\n| Write : Action\n",
    )
    .unwrap();
    std::fs::write(
        app.join("proofs/Spec.lean"),
        "/-! ## Policy -/\n\ndef policy : Role → Action → Bool\n  | .Writer, _ => true\n  | .Reader, .Read => true\n  | _, _ => false\n\n/-! ## Invariants -/\n\nstructure Inv (s : St) : Prop where\n  /-- Ids are unique. -/\n  keys : s.notes.Nodup\n",
    )
    .unwrap();
    std::fs::write(
        app.join("proofs/Theorems.lean"),
        "namespace notes_kernel.Theorems\n\n/-- Every write fits the policy. -/\ntheorem authorized : True := by\n  have := h.keys\n  trivial\n\n/-- False before the fix. -/\ntheorem pre_fix_broken : ¬ False := by simp\n\nend notes_kernel.Theorems\n",
    )
    .unwrap();
    std::fs::write(
        app.join("proofs/scope.toml"),
        "app = \"notes\"\ntitle = \"note service\"\nassumes = [\"A1\", \"A9\"]\n\n[[trusted_input]]\nname = \"clock\"\nfrom = \"Clock::System\"\n\n[[out_of_scope]]\nitem = \"liveness\"\nreason = \"no statement form\"\n\n[[counterexample]]\ntheorem = \"pre_fix_broken\"\nbug = \"issue #7\"\ncurrent = true\n",
    )
    .unwrap();
    std::fs::write(
        app.join("README.md"),
        "# notes\n\n## Theorems\n\n| Theorem | Statement |\n|---|---|\n| `authorized` | Every write is allowed by the policy. |\n| `pre_fix_broken` | Before the fix the statement is false. |\n",
    )
    .unwrap();
    std::fs::create_dir_all(repo.dir.join("docs/app")).unwrap();
    std::fs::write(
        repo.dir.join("docs/app/ROADMAP.md"),
        "## Assumption ledger\n\n| # | Assumption | Today | Target |\n|---|---|---|---|\n| A1 | Authenticator returns the real sender | proven token parser | keys trusted |\n| A9 | Running code is the extracted code | CI re-extracts | done |\n",
    )
    .unwrap();
    // The receipt names the proofs' digest, so "unchanged since" is true, and
    // one caught mutant whose failing proof names the `keys` clause.
    let digest = h5i_core::apps::digest_tree(&app.join("proofs"), "lean");
    let kernel = h5i_core::apps::digest_tree(&app.join("kernel"), "rs");
    std::fs::create_dir_all(repo.dir.join(".h5i/app-verify")).unwrap();
    let receipt = serde_json::json!({
        "version": 1, "started": "2026-10-02T10:00:00Z", "finished": "2026-10-02T10:05:00Z", "secs": 300.0,
        "git": {"head": "0123456789abcdef", "branch": "main", "dirty": false}, "full": true, "extract": true,
        "summary": {"passed": 3, "failed": 0, "skipped": 1},
        "steps": [
            {"name": "route coverage", "project": null, "outcome": "pass", "detail": "", "secs": 0.1},
            {"name": "lean examples/app/notes/proofs", "project": "examples/app/notes/proofs", "outcome": "pass", "detail": "", "secs": 40.0},
            {"name": "lean elsewhere", "project": "examples/app/other/proofs", "outcome": "fail", "detail": "boom", "secs": 1.0},
            {"name": "rust tests", "project": null, "outcome": "skip", "detail": "H5I_APP_TEST_DATABASE_URL unset", "secs": 0.0}
        ],
        "projects": [{"dir": "examples/app/notes/proofs", "app": "notes", "kernel": "examples/app/notes/kernel",
                       "kernel_digest": kernel, "proofs_digest": digest, "lean": "pass", "extraction": "match", "authorization": "theorem"}],
        "axioms": [{"dir": "examples/app/notes/proofs", "theorem": "notes_kernel.Theorems.authorized", "axioms": ["propext", "Classical.choice", "Quot.sound"], "verdict": "ok"}],
        "mutants": [
            {"suite": "apps", "name": "notes", "app": "examples/app/notes", "bug": "duplicate ids accepted", "expect": ["authorized"], "verdict": "caught", "stage": "lake",
             "failed": [{"file": "Theorems.lean", "line": 5, "col": 2, "message": "unsolved goals", "decl": "notes_kernel.Theorems.authorized", "kind": "theorem"}], "secs": 30.0}
        ],
        "difftest": null
    });
    std::fs::write(repo.dir.join(".h5i/app-verify/latest.json"), receipt.to_string()).unwrap();
}

#[test]
fn the_console_lists_proof_projects_and_reads_one_in_full() {
    let repo = Repo::new();
    write_proof_project(&repo);
    let ui = Console::start(&repo);

    let apps = ui.get_authed("/api/apps").json();
    let rows = apps.as_array().expect("an array");
    assert_eq!(rows.len(), 1, "one proof project, build output ignored: {apps}");
    let a = &rows[0];
    assert_eq!(a["dir"], "examples/app/notes/proofs");
    assert_eq!(a["app"], "notes");
    assert_eq!(a["kind"], "server");
    assert_eq!(a["theorems"], 2);
    assert_eq!(a["counterexamples"], 1);
    assert_eq!(a["receipt"]["proofs_match"], true);
    assert_eq!(a["receipt"]["kernel_match"], true);
    assert_eq!(a["receipt"]["mutants"]["caught"], 1);
    assert_eq!(a["receipt"]["axioms"]["ok"], 1);
    assert_eq!(a["flags"], serde_json::json!([]), "a current receipt with nothing failing raises no flag");

    let d = ui.get_authed("/api/app/examples/app/notes/proofs").json();
    // The spec: a policy matrix over the generated enums, and the invariant
    // clause with the mutant whose failing proof named it.
    let policy = &d["spec"]["sections"][0]["items"][0];
    assert_eq!(policy["matrix"]["rows"], serde_json::json!(["Reader", "Writer"]));
    assert_eq!(policy["matrix"]["cells"][0], serde_json::json!(["true", "false"]));
    let inv = &d["spec"]["sections"][1]["items"][0];
    assert_eq!(inv["fields"][0]["name"], "keys");
    assert_eq!(inv["fields"][0]["doc"], "Ids are unique.");
    assert_eq!(inv["fields"][0]["mutants"], serde_json::json!(["notes"]));
    // Theorems: the README statement, the gate's axioms, the mutant, and the
    // counterexample tied to its upstream bug.
    let th = d["theorems"].as_array().unwrap();
    let authorized = th.iter().find(|t| t["name"] == "authorized").unwrap();
    assert_eq!(authorized["statement"], "Every write is allowed by the policy.");
    assert_eq!(authorized["qualified"], "notes_kernel.Theorems.authorized");
    assert_eq!(authorized["axioms"]["verdict"], "ok");
    assert_eq!(authorized["caught"], serde_json::json!(["notes"]));
    assert_eq!(authorized["counterexample"], false);
    let broken = th.iter().find(|t| t["name"] == "pre_fix_broken").unwrap();
    assert_eq!(broken["counterexample"], true);
    assert_eq!(broken["bug"]["bug"], "issue #7");
    assert_eq!(broken["bug"]["current"], true);
    // The trust boundary: ledger text resolved, the opt-out found, the version
    // compared.
    assert_eq!(d["trust"]["assumes"][0]["id"], "A1");
    assert_eq!(d["trust"]["assumes"][0]["text"], "Authenticator returns the real sender");
    assert_eq!(d["trust"]["edges"][0]["kind"], "no-actor");
    assert_eq!(d["trust"]["trusted_inputs"][0]["name"], "clock");
    assert_eq!(d["trust"]["out_of_scope"][0]["item"], "liveness");
    assert_eq!(d["trust"]["version"]["proofs_match"], true);
    assert_eq!(d["trust"]["version"]["receipt_head"], "0123456789abcdef");
    // Evidence: this project's steps plus the global ones, never another
    // project's.
    let steps = d["evidence"]["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 3, "{steps:?}");
    assert!(steps.iter().all(|s| s["name"] != "lean elsewhere"));
    assert_eq!(d["evidence"]["mutants"][0]["as_expected"], true);
    assert_eq!(d["evidence"]["counts"]["prepared"], 1);

    assert_eq!(ui.get_authed("/api/app/examples/app/nope/proofs").status, 404);
    assert_eq!(ui.get_authed("/api/app/examples/app/notes/../notes/proofs").status, 404);
}

#[test]
fn proofs_become_a_checklist_and_a_finding_can_point_at_a_theorem() {
    let repo = Repo::new();
    write_proof_project(&repo);
    repo.h5i_ok(&["project", "init", "notes", "--title", "note service"]);
    repo.h5i_ok(&["project", "checklist", "import", "-p", "notes", "--proofs", "examples/app/notes/proofs", "--required"]);
    repo.h5i_ok(&[
        "project", "finding", "create", "-p", "notes", "--title", "Reader wrote a note", "--severity", "high",
        "--proof-theorem", "authorized", "--proof-relation", "in-scope", "--proof-version", "unconfirmed",
        "--proof-note", "staging build commit unknown",
    ]);
    // A later update changes one state and keeps the rest.
    repo.h5i_ok(&["project", "finding", "update", "F-1", "-p", "notes", "--proof-version", "differs"]);
    // The relation vocabulary is checked.
    let bad = repo.h5i(&["project", "finding", "update", "F-1", "-p", "notes", "--proof-relation", "maybe"]);
    assert!(!bad.status.success());
    assert!(String::from_utf8_lossy(&bad.stderr).contains("proof relation"));

    let ui = Console::start(&repo);
    let detail = ui.get_authed("/api/project/notes").json();
    let c = &detail["checklists"][0]["checklist"];
    assert_eq!(c["slug"], "proofs-notes");
    assert_eq!(c["mode"], "required");
    assert!(c["source"].as_str().unwrap().starts_with("proofs:examples/app/notes/proofs"));
    let texts: Vec<String> = c["items"].as_array().unwrap().iter().map(|i| i["text"].as_str().unwrap().to_string()).collect();
    assert!(texts.iter().any(|t| t.starts_with("`authorized`: Every write is allowed by the policy. Assumes A1, A9.")), "{texts:?}");
    assert!(texts.iter().any(|t| t.contains("pre_fix_broken") && t.contains("issue #7")), "{texts:?}");
    assert!(texts.iter().any(|t| t.contains("no-actor")), "{texts:?}");
    assert!(texts.iter().any(|t| t.starts_with("A1: Authenticator returns the real sender")), "{texts:?}");
    assert!(texts.iter().any(|t| t.contains("Kernel digest at import")), "{texts:?}");
    let groups: std::collections::BTreeSet<String> =
        c["items"].as_array().unwrap().iter().map(|i| i["group"].as_str().unwrap().to_string()).collect();
    assert!(groups.iter().any(|g| g.starts_with("Theorems")), "{groups:?}");
    assert!(groups.iter().any(|g| g.starts_with("Version")), "{groups:?}");

    let f = &detail["findings"][0];
    assert_eq!(f["proof"]["theorem"], "authorized");
    assert_eq!(f["proof"]["relation"], "in-scope");
    assert_eq!(f["proof"]["version"], "differs");
    assert_eq!(f["proof"]["note"], "staging build commit unknown");
}
