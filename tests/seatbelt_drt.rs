//! Differential testing of the Seatbelt profile generator (ROADMAP.md §V3,
//! the Seatbelt refinement): `seatbelt::build_profile` is pure and compiles
//! on Linux, so this harness runs it here, parses the **file rules** out of
//! the generated SBPL text, and diffs them — structurally, rule for rule,
//! path for path — against the Lean model's emission
//! (`lean/H5iSpec/Seatbelt.lean`, via `h5i-spec --seatbelt`). The theorems
//! (`fs_deny_wins`: the deny tail beats every grant, SBPL being last-match-
//! wins) are proved against the same emission, so a green diff carries them
//! to the real generator.
//!
//! Scope, matching the model's: forms whose operations are exactly
//! `file-read*`/`file-write*` sets. Network, mach, sysctl, signal, ioctl
//! and metadata forms are excluded on both sides. `macos_developer_reads`
//! is host-measured (empty on Linux — an on-mac sweep would exercise it;
//! named gap) and `config_lock_paths` is measured by the harness the same
//! way the generator measures it.
//!
//! Skips loudly without the Lean binary; `H5I_DRT_REQUIRE=1` (the Lean CI
//! lane) turns absence into failure.

#![cfg(unix)]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use h5i_core::sandbox_policy::{
    HomeBind, IsolationClaim, Profile, ResolvedPolicy, RoBind,
};
use h5i_core::seatbelt::{self, SeatbeltOptions};
use serde_json::{json, Value};

fn lean_bin() -> Option<PathBuf> {
    let path = std::env::var_os("H5I_SPEC_BIN").map(PathBuf::from).unwrap_or_else(|| {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("lean/.lake/build/bin/h5i-spec")
    });
    if path.is_file() {
        return Some(path);
    }
    let msg = format!(
        "Lean model binary not found at {} — build it with `cd lean && lake build`",
        path.display()
    );
    if std::env::var_os("H5I_DRT_REQUIRE").is_some_and(|v| v == "1") {
        panic!("{msg} (H5I_DRT_REQUIRE=1)");
    }
    eprintln!("SKIPPING seatbelt DRT: {msg}");
    None
}

/// One parsed SBPL form: decision, operation names, filter kind, paths.
#[derive(Debug, PartialEq)]
struct SbplRule {
    decision: String,
    ops: Vec<String>,
    kind: String,
    paths: Vec<String>,
}

fn rule_value(r: &SbplRule) -> Value {
    json!({"decision": r.decision, "ops": r.ops, "kind": r.kind, "paths": r.paths})
}

/// Extract the top-level `(...)` forms from SBPL text.
fn top_level_forms(text: &str) -> Vec<String> {
    let mut forms = Vec::new();
    let mut depth = 0usize;
    let mut cur = String::new();
    let mut in_str = false;
    let mut escaped = false;
    for line in text.lines() {
        if depth == 0 && (line.starts_with(";;") || line.trim().is_empty()) {
            continue;
        }
        for c in line.chars() {
            if depth > 0 {
                cur.push(c);
            }
            match c {
                '"' if !escaped => in_str = !in_str,
                '\\' if in_str && !escaped => {
                    escaped = true;
                    continue;
                }
                '(' if !in_str => {
                    if depth == 0 {
                        cur.push(c);
                    }
                    depth += 1;
                }
                ')' if !in_str => {
                    depth -= 1;
                    if depth == 0 {
                        forms.push(std::mem::take(&mut cur));
                    }
                }
                _ => {}
            }
            escaped = false;
        }
        if depth > 0 {
            cur.push('\n');
        }
    }
    assert_eq!(depth, 0, "unbalanced SBPL");
    forms
}

/// Parse one form into a rule, if it is a file rule in the model's scope.
fn parse_file_rule(form: &str) -> Option<SbplRule> {
    let inner = form.strip_prefix('(')?.strip_suffix(')')?;
    let head_end = inner.find('(').unwrap_or(inner.len());
    let words: Vec<&str> = inner[..head_end].split_whitespace().collect();
    let (decision, ops) = words.split_first()?;
    if !matches!(*decision, "allow" | "deny") {
        return None;
    }
    let ops: Vec<String> = ops.iter().map(|s| s.to_string()).collect();
    // The model's scope: pure file-read*/file-write* operation sets.
    if ops.is_empty() || !ops.iter().all(|o| o == "file-read*" || o == "file-write*") {
        return None;
    }
    // Filter clauses: (subpath "p") / (literal "p") / (regex #"p").
    let mut kind: Option<&str> = None;
    let mut paths = Vec::new();
    let mut rest = &inner[head_end..];
    while let Some(open) = rest.find('(') {
        let close = find_matching(rest, open);
        let clause = &rest[open + 1..close];
        rest = &rest[close + 1..];
        let (k, arg) = clause.split_once(char::is_whitespace)?;
        let arg = arg.trim();
        let path = if let Some(stripped) = arg.strip_prefix("#\"") {
            assert_eq!(k, "regex");
            stripped.strip_suffix('"')?.to_string()
        } else {
            unescape(arg.strip_prefix('"')?.strip_suffix('"')?)
        };
        match kind {
            None => kind = Some(k),
            Some(prev) => assert_eq!(
                prev, k,
                "mixed filter kinds in a file rule — outside the model's scope: {form}"
            ),
        }
        paths.push(path);
    }
    Some(SbplRule {
        decision: decision.to_string(),
        ops,
        kind: kind?.to_string(),
        paths,
    })
}

fn find_matching(s: &str, open: usize) -> usize {
    let mut depth = 0usize;
    let mut in_str = false;
    let mut escaped = false;
    for (i, c) in s.char_indices().skip(open) {
        match c {
            '"' if !escaped => in_str = !in_str,
            '\\' if in_str && !escaped => {
                escaped = true;
                continue;
            }
            '(' if !in_str => depth += 1,
            ')' if !in_str => {
                depth -= 1;
                if depth == 0 {
                    return i;
                }
            }
            _ => {}
        }
        escaped = false;
    }
    panic!("unbalanced clause in {s}");
}

fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(n) = chars.next() {
                out.push(n);
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The harness's own measurement of `config_lock_paths`, mirroring the
/// generator's: project dirs that exist, then home config files that exist.
fn measured_config_locks(work: &Path, home: Option<&Path>) -> Vec<String> {
    let mut out = Vec::new();
    for dir in [".claude", ".codex"] {
        let p = work.join(dir);
        if p.is_dir() {
            out.push(p.to_string_lossy().into_owned());
        }
    }
    if let Some(home) = home {
        for file in [".claude/settings.json", ".codex/config.toml"] {
            let p = home.join(file);
            if p.is_file() {
                out.push(p.to_string_lossy().into_owned());
            }
        }
    }
    out
}

/// Host-measured developer reads — empty on Linux (the paths do not
/// exist), which the harness measures the same way the generator does.
fn measured_developer_reads() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |p: PathBuf| {
        let s = p.to_string_lossy().into_owned();
        if p.is_dir() && !out.contains(&s) {
            out.push(s);
        }
    };
    if let Ok(active) = std::fs::read_link("/var/db/xcode_select_link") {
        push(active);
    }
    for d in ["/Applications/Xcode.app/Contents/Developer", "/Library/Developer/CommandLineTools"] {
        push(PathBuf::from(d));
    }
    out
}

fn gen_policy(rng: &mut fastrand::Rng, root: &Path) -> (ResolvedPolicy, PathBuf) {
    let claim = IsolationClaim::Process;
    let mut profile = Profile::builtin("default", claim);
    // Paths chosen to exercise the alias machinery: /tmp-, /var-, /private-
    // prefixed spellings, tildes, and $-prefixed lint entries.
    let pool = [
        "/tmp/shared", "/var/cache/x", "/private/tmp/y", "/etc/ssl", "/opt/x",
        "~/.cargo/registry", "~/.cache", "$REPO/.git/hooks", "/usr/local/lib",
    ];
    let pick = |rng: &mut fastrand::Rng, max: usize| -> Vec<String> {
        (0..rng.usize(0..=max)).map(|_| pool[rng.usize(0..pool.len())].to_string()).collect()
    };
    profile.fs_read = pick(rng, 4);
    profile.fs_write = pick(rng, 3);
    if rng.bool() {
        profile.fs_write.push("$WORK".into());
    }
    profile.fs_deny = pick(rng, 3);
    let mut policy = ResolvedPolicy::new(claim, profile);
    policy.work_readonly = rng.bool();
    policy.ro_binds = (0..rng.usize(0..=2))
        .map(|i| RoBind { backing: root.join(format!("cache{i}")), target: root.join(format!("ct{i}")) })
        .collect();
    policy.cache_write = rng.bool().then(|| RoBind {
        backing: root.join("cw-backing"),
        target: root.join("cw-target"),
    });
    policy.env_capture_spool = rng.bool().then(|| root.join("spool"));
    policy.home_binds = (0..rng.usize(0..=2))
        .map(|i| HomeBind {
            backing: root.join(format!("hb{i}")),
            target: if rng.bool() {
                PathBuf::from("/tmp")
            } else {
                root.join(format!("shadow{i}"))
            },
        })
        .collect();
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    (policy, work)
}

#[test]
fn rust_generator_and_lean_model_emit_the_same_file_rules() {
    let Some(bin) = lean_bin() else { return };
    let seed: u64 =
        std::env::var("H5I_DRT_SEED").ok().and_then(|s| s.parse().ok()).unwrap_or(0x5EA7_BE17);
    let mut rng = fastrand::Rng::with_seed(seed);
    let tmp = tempfile::tempdir().unwrap();
    let mut mismatches = 0;
    for case in 0..100 {
        let root = tmp.path().join(format!("case{case}"));
        std::fs::create_dir_all(&root).unwrap();
        let (policy, work) = gen_policy(&mut rng, &root);
        let interactive = rng.bool();
        // A deterministic, purely-passed home: the generator never reads the
        // env for it, and neither do we.
        let home = rng.bool().then(|| root.join("home"));
        if let Some(h) = &home {
            std::fs::create_dir_all(h).unwrap();
        }
        if interactive && rng.bool() {
            // Materialize some config-lock state so the measured list varies.
            std::fs::create_dir_all(work.join(".claude")).unwrap();
            if let Some(h) = &home {
                std::fs::create_dir_all(h.join(".codex")).unwrap();
                std::fs::write(h.join(".codex/config.toml"), b"").unwrap();
            }
        }
        let opts = SeatbeltOptions {
            proxy_ports: Vec::new(),
            interactive,
            home: home.clone(),
        };

        // The Rust side: generate, parse the file rules back out.
        let sbpl = seatbelt::build_profile(&policy, &work, &opts);
        let rust_rules: Vec<SbplRule> =
            top_level_forms(&sbpl).iter().filter_map(|f| parse_file_rule(f)).collect();

        // The Lean side: the same inputs, host measurements included.
        let p = &policy.profile;
        let input = json!({
            "fs_read": p.fs_read,
            "fs_write": p.fs_write,
            "fs_deny": p.fs_deny,
            "work": work.to_string_lossy(),
            "work_readonly": policy.work_readonly,
            "home": home.as_ref().map(|h| h.to_string_lossy()),
            "interactive": interactive,
            "ro_backings": policy.ro_binds.iter()
                .map(|b| b.backing.to_string_lossy()).collect::<Vec<_>>(),
            "cache_write_backing": policy.cache_write.as_ref()
                .map(|b| b.backing.to_string_lossy()),
            "capture_spool": policy.env_capture_spool.as_ref()
                .map(|s| s.to_string_lossy()),
            "shadowed_home": policy.home_binds.iter()
                .filter(|b| b.target != Path::new("/tmp"))
                .map(|b| b.target.to_string_lossy()).collect::<Vec<_>>(),
            "developer_reads": measured_developer_reads(),
            "config_locks": measured_config_locks(&work, home.as_deref()),
        });
        let mut child = Command::new(&bin)
            .arg("--seatbelt")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn lean model");
        child.stdin.take().unwrap().write_all(input.to_string().as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "lean --seatbelt exited with {:?}", out.status);
        let lean_rules: Value = serde_json::from_slice(&out.stdout).unwrap();
        let rust_value = Value::Array(rust_rules.iter().map(rule_value).collect());
        if rust_value != lean_rules {
            mismatches += 1;
            eprintln!(
                "SEATBELT MISMATCH case {case} (seed {seed}):\ninput: {input}\nrust: {rust_value}\nlean: {lean_rules}\nsbpl:\n{sbpl}\n"
            );
        }
    }
    assert_eq!(mismatches, 0, "{mismatches} seatbelt mismatches (seed {seed})");
}
