//! M8 against the real thing.
//!
//! The unit tests in `browser_proxy` prove the policy path with fake streams.
//! This proves the part that only a real daemon can: that the actual
//! `agent-browser` CLI, unmodified, works through an h5i mediator sitting on
//! the socket it was told to use, and is refused when h5i says no.
//!
//! Skipped (loudly) when the host has no agent-browser or no Chrome, because a
//! test that silently passes on a machine that cannot run it is worse than one
//! that says why it did not.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use h5i_core::browser_proxy::{self, ActionPolicy};

/// `sun_path` is 108 bytes, and a socket dir plus `default.sock` gets there
/// faster than expected, so these live at the top of `/tmp`, not under a
/// nested temp dir.
fn short_dir(tag: &str) -> PathBuf {
    let dir = PathBuf::from(format!("/tmp/h5i-med-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn agent_browser() -> Option<PathBuf> {
    for candidate in [
        "/usr/local/bin/agent-browser",
        "/usr/bin/agent-browser",
        "/opt/homebrew/bin/agent-browser",
    ] {
        let path = PathBuf::from(candidate);
        if path.exists() {
            return Some(path);
        }
    }
    let home = std::env::var("HOME").ok()?;
    for rel in [".cargo/bin/agent-browser", ".local/bin/agent-browser"] {
        let path = Path::new(&home).join(rel);
        if path.exists() {
            return Some(path);
        }
    }
    None
}

/// Run the CLI against a socket dir, returning (stdout+stderr, success).
fn run_cli(binary: &Path, socket_dir: &Path, args: &[&str]) -> (String, bool) {
    let out = Command::new(binary)
        .args(args)
        .env("AGENT_BROWSER_SOCKET_DIR", socket_dir)
        // Identical on both sides: the daemon's config fingerprint covers its
        // options, so a mismatch here makes the CLI decide the daemon is stale
        // and try to replace it.
        .env("AGENT_BROWSER_ARGS", "--no-sandbox --disable-dev-shm-usage")
        .output()
        .expect("agent-browser runs");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (text, out.status.success())
}

#[test]
fn the_real_cli_is_mediated_and_refused_through_an_h5i_socket() {
    let Some(cli) = agent_browser() else {
        eprintln!("SKIP: no agent-browser on this host");
        return;
    };

    let daemon_dir = short_dir("d"); // where h5i would run the real daemon
    let front_dir = short_dir("f"); // what the box is told to use
    let env_dir = short_dir("e"); // stands in for the box's env dir

    // A page to look at, so `read` has something real to return.
    let page = daemon_dir.join("page.html");
    let mut file = std::fs::File::create(&page).expect("page");
    write!(file, "<!doctype html><title>Mediated</title><h1>Hello from the daemon</h1>")
        .expect("write page");

    // 1. Start the real daemon on the private path.
    let (out, ok) = run_cli(
        &cli,
        &daemon_dir,
        &["open", &format!("file://{}", page.display())],
    );
    if !ok || !daemon_dir.join("default.sock").exists() {
        eprintln!("SKIP: could not start a real daemon on this host: {out}");
        let _ = std::fs::remove_dir_all(&daemon_dir);
        return;
    }

    // 2. Mirror the sibling files the CLI checks before deciding a daemon is
    //    up. Without these it concludes there is none and starts its own.
    for name in ["default.version", "default.config", "default.stream"] {
        let _ = std::fs::copy(daemon_dir.join(name), front_dir.join(name));
    }

    // 3. h5i takes the socket the box can reach.
    let mediator = browser_proxy::spawn(
        &front_dir.join("default.sock"),
        &daemon_dir.join("default.sock"),
        &env_dir,
        ActionPolicy::deny_all_of(["evaluate"]),
    )
    .expect("mediator starts");

    // 4. A read passes through and comes back from the real browser.
    let (read_out, read_ok) = run_cli(&cli, &front_dir, &["read"]);
    assert!(read_ok, "read should succeed through the mediator: {read_out}");
    assert!(
        read_out.contains("Hello from the daemon"),
        "the mediated read must return the real page: {read_out}"
    );

    // 5. A denied action is refused by h5i, and never reaches the browser.
    let (eval_out, _) = run_cli(&cli, &front_dir, &["eval", "1+1"]);
    assert!(
        !eval_out.contains("\n2") && !eval_out.trim().ends_with('2'),
        "eval must not have been evaluated: {eval_out}"
    );
    assert!(
        eval_out.contains("denied") || eval_out.contains("fail-closed"),
        "the agent should see h5i's refusal: {eval_out}"
    );

    // 6. The mediator recorded both, which is what reaches the receipt.
    std::thread::sleep(std::time::Duration::from_millis(200));
    let actions = mediator.actions();
    assert!(
        actions.iter().any(|a| a.action == "read" && a.forwarded),
        "{actions:?}"
    );
    assert!(
        actions
            .iter()
            .any(|a| a.action == "evaluate" && !a.forwarded),
        "{actions:?}"
    );

    drop(mediator);
    let _ = run_cli(&cli, &daemon_dir, &["close"]);
    for dir in [&daemon_dir, &front_dir, &env_dir] {
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
fn a_human_takeover_stops_the_real_cli_from_clicking() {
    let Some(cli) = agent_browser() else {
        eprintln!("SKIP: no agent-browser on this host");
        return;
    };

    let daemon_dir = short_dir("d2");
    let front_dir = short_dir("f2");
    let env_dir = short_dir("e2");

    let page = daemon_dir.join("page.html");
    std::fs::write(
        &page,
        "<!doctype html><title>T</title><button id=b>Go</button>",
    )
    .expect("page");

    let (out, ok) = run_cli(
        &cli,
        &daemon_dir,
        &["open", &format!("file://{}", page.display())],
    );
    if !ok || !daemon_dir.join("default.sock").exists() {
        eprintln!("SKIP: could not start a real daemon on this host: {out}");
        let _ = std::fs::remove_dir_all(&daemon_dir);
        return;
    }
    for name in ["default.version", "default.config", "default.stream"] {
        let _ = std::fs::copy(daemon_dir.join(name), front_dir.join(name));
    }

    // The human takes the lock. The case the lock existed for and nothing
    // enforced against the agent until now.
    h5i_core::control::take(&env_dir).expect("human takes control");

    let mediator = browser_proxy::spawn(
        &front_dir.join("default.sock"),
        &daemon_dir.join("default.sock"),
        &env_dir,
        ActionPolicy::default(),
    )
    .expect("mediator starts");

    let (click_out, _) = run_cli(&cli, &front_dir, &["click", "#b"]);
    assert!(
        click_out.to_lowercase().contains("human")
            || click_out.to_lowercase().contains("control"),
        "the agent must be told the human holds control: {click_out}"
    );

    // Watching never collides, so a read still works during the takeover.
    let (read_out, read_ok) = run_cli(&cli, &front_dir, &["read"]);
    assert!(
        read_ok,
        "read-only verbs must survive a takeover: {read_out}"
    );

    std::thread::sleep(std::time::Duration::from_millis(200));
    let actions = mediator.actions();
    assert!(
        actions
            .iter()
            .any(|a| a.action == "click" && !a.forwarded),
        "the click must be recorded as refused: {actions:?}"
    );

    drop(mediator);
    let _ = run_cli(&cli, &daemon_dir, &["close"]);
    for dir in [&daemon_dir, &front_dir, &env_dir] {
        let _ = std::fs::remove_dir_all(dir);
    }
}
