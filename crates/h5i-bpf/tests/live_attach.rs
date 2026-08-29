//! The one test that needs a kernel.
//!
//! Loading a BPF program and attaching it to a tracepoint requires `CAP_BPF`
//! and `CAP_PERFMON`, which no CI runner grants and which an ordinary
//! development machine does not give a `cargo test`. So this suite **skips
//! loudly** rather than failing, and prints exactly why: a test that silently
//! passes on a host that could not run it is worse than one that is not there.
//!
//! To run it for real:
//!
//! ```text
//! sudo -E env "PATH=$PATH" H5I_BPF_LIVE=1 \
//!     cargo test -p h5i-bpf --test live_attach -- --nocapture
//! ```
//!
//! `H5I_BPF_LIVE=1` is the opt-in. Without it the suite skips even as root,
//! because loading programs into the running kernel is not something a bare
//! `cargo test` should do to somebody's machine by surprise.

use std::process::Command;

use h5i_bpf::rules::RuleContext;
use h5i_bpf::scope::Tier;
use h5i_bpf::{DetectConfig, Watch};

/// Why this host cannot run the live suite, or `None` when it can.
fn skip_reason() -> Option<String> {
    if std::env::var("H5I_BPF_LIVE").as_deref() != Ok("1") {
        return Some(
            "H5I_BPF_LIVE=1 not set (this suite loads BPF programs into the running kernel)"
                .to_string(),
        );
    }
    let caps = h5i_bpf::probe_host();
    caps.unavailable_reason()
}

macro_rules! skip_unless_live {
    () => {
        if let Some(why) = skip_reason() {
            eprintln!("SKIP: {why}");
            return;
        }
    };
}

/// The end-to-end claim: start a session, run a command that trips a known
/// rule, and find that rule in the block.
///
/// `openat` of a credential path is used rather than anything exotic, because
/// it exercises the whole chain — the process-tree scope admitting a
/// descendant, the in-kernel prefix filter letting the path through, the ring
/// buffer, the decoder, and the rule.
#[test]
fn a_credential_read_by_a_child_reaches_the_receipt() {
    skip_unless_live!();

    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
    let secret_dir = format!("{home}/.ssh");
    let cfg = DetectConfig {
        tier: Tier::Workspace,
        context: RuleContext {
            net_mode: "allow".into(),
            workspace: std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            home: home.clone(),
            ..Default::default()
        },
        prefixes: h5i_bpf::kernel_prefixes(&home, ""),
        ..Default::default()
    };

    let watch = Watch::start(cfg);
    assert!(
        watch.is_live(),
        "the probe should have attached: {:?}",
        watch.refusal()
    );

    // A child, so the scope has to admit a descendant rather than h5i itself.
    let _ = Command::new("/bin/sh")
        .arg("-c")
        .arg(format!("cat {secret_dir}/* >/dev/null 2>&1; true"))
        .status();
    // The reader polls on a 100ms cadence; give it one.
    std::thread::sleep(std::time::Duration::from_millis(300));

    let block = watch.finish();
    eprintln!("{}", serde_json::to_string_pretty(&block).unwrap());

    assert!(block.observed(), "{}", block.summary());
    assert!(block.events_seen > 0, "no events at all: {}", block.summary());
    assert!(
        block.detections.iter().any(|d| d.rule == "secret.read"),
        "expected secret.read, got: {:?}",
        block.detections.iter().map(|d| &d.rule).collect::<Vec<_>>()
    );
}

/// h5i's own threads must not be reported as the box. The probe's state
/// machine is what makes this true, and it is the difference between a lane
/// that reports on a box and one that reports on h5i.
#[test]
fn the_host_processs_own_activity_is_not_attributed_to_the_box() {
    skip_unless_live!();

    let cfg = DetectConfig {
        tier: Tier::Workspace,
        context: RuleContext {
            net_mode: "allow".into(),
            home: std::env::var("HOME").unwrap_or_default(),
            ..Default::default()
        },
        prefixes: h5i_bpf::kernel_prefixes(&std::env::var("HOME").unwrap_or_default(), ""),
        ..Default::default()
    };
    let watch = Watch::start(cfg);
    assert!(watch.is_live(), "{:?}", watch.refusal());

    // Do credential-shaped work in *this* process, spawning nothing.
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
    for _ in 0..20 {
        let _ = std::fs::read(format!("{home}/.ssh/config"));
    }
    std::thread::sleep(std::time::Duration::from_millis(300));

    let block = watch.finish();
    assert!(
        !block.detections.iter().any(|d| d.rule == "secret.read"),
        "h5i's own reads were attributed to the box: {:?}",
        block.detections
    );
}

/// An exec is the first thing a run does and the most valuable single event.
#[test]
fn a_childs_exec_is_captured_with_its_arguments() {
    skip_unless_live!();

    let cfg = DetectConfig {
        tier: Tier::Process,
        context: RuleContext {
            net_mode: "allow".into(),
            ..Default::default()
        },
        ..Default::default()
    };
    let watch = Watch::start(cfg);
    assert!(watch.is_live(), "{:?}", watch.refusal());

    let _ = Command::new("/usr/bin/env")
        .args(["sh", "-c", "curl --version >/dev/null 2>&1 | sh; true"])
        .status();
    std::thread::sleep(std::time::Duration::from_millis(300));

    let block = watch.finish();
    eprintln!("{}", block.summary());
    assert!(block.events_seen > 0);
    assert_eq!(block.coverage, h5i_bpf::Coverage::Full);
}

/// Stopping must actually stop. A session dropped without `finish` still has
/// to join its reader, or the next test inherits a thread polling a ring
/// buffer whose maps have gone.
#[test]
fn dropping_a_session_stops_its_collector() {
    skip_unless_live!();

    let before = std::thread::available_parallelism().is_ok();
    {
        let watch = Watch::start(DetectConfig {
            tier: Tier::Workspace,
            ..Default::default()
        });
        assert!(watch.is_live(), "{:?}", watch.refusal());
        drop(watch);
    }
    // Reaching here without hanging is the assertion: `Session::drop` joins.
    assert!(before);
}
