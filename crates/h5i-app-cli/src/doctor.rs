//! `h5i app doctor`: are the tools installed, at the pinned versions, and
//! does the project use the same pins?

use std::path::Path;
use std::process::Command;

use anyhow::{Result, bail};

use crate::manifest::Project;
use crate::pins;
use crate::util;

#[derive(Default)]
struct Report {
    failed: usize,
    warned: usize,
}

impl Report {
    fn ok(&self, what: &str, detail: &str) {
        eprintln!("  ok    {what:<22} {detail}");
    }
    fn warn(&mut self, what: &str, detail: &str) {
        self.warned += 1;
        eprintln!("  warn  {what:<22} {detail}");
    }
    fn fail(&mut self, what: &str, detail: &str) {
        self.failed += 1;
        eprintln!("  FAIL  {what:<22} {detail}");
    }
}

fn first_line(cmd: &mut Command) -> Option<String> {
    util::capture(cmd)
        .ok()
        .and_then(|s| s.lines().next().map(|l| l.trim().to_string()))
}

/// The 40-hex commit inside a version string, or a shorter hex prefix.
fn commit_in(s: &str) -> Option<String> {
    s.split(|c: char| !c.is_ascii_hexdigit())
        .filter(|w| w.len() >= 7)
        .max_by_key(|w| w.len())
        .map(str::to_lowercase)
}

fn same_commit(found: &str, pin: &str) -> bool {
    pin.starts_with(found) || found.starts_with(pin)
}

pub fn doctor(project: Option<&Project>) -> Result<()> {
    let mut r = Report::default();
    eprintln!("tools");
    match util::on_path("cargo") {
        Some(_) => r.ok(
            "cargo",
            &first_line(Command::new("cargo").arg("--version")).unwrap_or_default(),
        ),
        None => r.fail("cargo", "not on PATH: install Rust from https://rustup.rs"),
    }
    let install = format!(
        "nix build github:AeneasVerif/aeneas/{} (and #charon), or build both from source at that commit",
        &pins::AENEAS_REV[..12]
    );
    match util::on_path("charon") {
        None => r.fail("charon", &format!("not on PATH: {install}")),
        Some(_) => {
            let v = first_line(Command::new("charon").arg("version")).unwrap_or_default();
            match commit_in(&v) {
                Some(c) if same_commit(&c, pins::CHARON_REV) => r.ok("charon", &v),
                Some(_) => r.fail(
                    "charon",
                    &format!("{v}; this h5i expects {}", &pins::CHARON_REV[..12]),
                ),
                None => r.warn(
                    "charon",
                    &format!("cannot read its commit from `charon version`: {v:?}"),
                ),
            }
        }
    }
    match util::on_path("aeneas") {
        None => r.fail("aeneas", &format!("not on PATH: {install}")),
        Some(_) => {
            let v = first_line(Command::new("aeneas").arg("-version")).unwrap_or_default();
            match commit_in(&v) {
                Some(c) if same_commit(&c, pins::AENEAS_REV) => r.ok("aeneas", &v),
                Some(_) => r.fail(
                    "aeneas",
                    &format!("{v}; this h5i expects {}", &pins::AENEAS_REV[..12]),
                ),
                None => r.warn(
                    "aeneas",
                    &format!("cannot read its commit from `aeneas -version`: {v:?}"),
                ),
            }
        }
    }
    match (util::on_path("lake"), util::on_path("elan")) {
        (Some(_), _) => r.ok(
            "lake",
            "on PATH (elan picks Lean from each project's lean-toolchain)",
        ),
        (None, _) => r.fail(
            "lake",
            "not on PATH: install elan from https://github.com/leanprover/elan",
        ),
    }

    if let Some(p) = project {
        eprintln!("project {}", p.display());
        let proofs = p.proofs();
        match std::fs::read_to_string(proofs.join("lean-toolchain")) {
            Ok(t) if t.trim() == pins::LEAN_TOOLCHAIN => r.ok("lean-toolchain", t.trim()),
            Ok(t) => r.fail(
                "lean-toolchain",
                &format!(
                    "{}; Aeneas {} needs {}",
                    t.trim(),
                    &pins::AENEAS_REV[..8],
                    pins::LEAN_TOOLCHAIN
                ),
            ),
            Err(_) => r.fail(
                "lean-toolchain",
                &format!("missing in {}", proofs.display()),
            ),
        }
        check_lakefile(&mut r, &proofs);
        let pk = crate::packages::Packages::locate(&proofs, crate::packages::cache_root().as_deref())?;
        let at = if pk.shared {
            format!("shared, in {}", crate::manifest::display_path(&pk.dir))
        } else {
            crate::manifest::display_path(&pk.dir)
        };
        match pk.state() {
            crate::packages::State::Ready => r.ok("lake packages", &at),
            crate::packages::State::Incomplete => r.warn(
                "lake packages",
                &format!("{at}: fetch unfinished; `h5i app check` resumes it, `--refetch` starts over"),
            ),
            crate::packages::State::Missing => r.warn(
                "lake packages",
                &format!("not fetched yet; `h5i app check` fetches them ({at})"),
            ),
        }
        let pin = pk.dir.join("aeneas/charon-pin");
        if let Ok(text) = std::fs::read_to_string(&pin) {
            let c = text
                .lines()
                .map(str::trim)
                .find(|l| !l.starts_with('#') && !l.is_empty())
                .unwrap_or("");
            if c != pins::CHARON_REV {
                r.fail(
                    "charon-pin",
                    &format!(
                        "the fetched Aeneas wants Charon {c}, this h5i pins {}",
                        pins::CHARON_REV
                    ),
                );
            }
        }
        if let Some(ex) = &p.manifest.extract {
            let krate = p.root.join(&ex.krate);
            if krate.join("Cargo.toml").is_file() {
                r.ok("kernel crate", &crate::manifest::display_path(&krate));
            } else {
                r.fail(
                    "kernel crate",
                    &format!("no Cargo.toml in {}", krate.display()),
                );
            }
            let generated = p.generated();
            let any = std::fs::read_dir(&generated).map(|d| {
                d.flatten()
                    .any(|e| e.path().extension().is_some_and(|x| x == "lean"))
            });
            if matches!(any, Ok(true)) {
                r.ok("extraction", &crate::manifest::display_path(&generated));
            } else {
                r.warn(
                    "extraction",
                    "nothing in proofs/generated yet; run `h5i app extract`",
                );
            }
        }
    }
    eprintln!("{} failed, {} warnings", r.failed, r.warned);
    if r.failed > 0 {
        bail!("doctor found {} problem(s)", r.failed);
    }
    Ok(())
}

fn check_lakefile(r: &mut Report, proofs: &Path) {
    let Ok(text) = std::fs::read_to_string(proofs.join("lakefile.lean")) else {
        r.fail("lakefile.lean", "missing");
        return;
    };
    let line = text
        .lines()
        .skip_while(|l| !l.contains("require aeneas"))
        .take(2)
        .collect::<Vec<_>>()
        .join(" ");
    if line.is_empty() {
        r.warn("aeneas require", "lakefile.lean does not require aeneas");
    } else if line.contains(pins::AENEAS_REV) {
        r.ok("aeneas require", &pins::AENEAS_REV[..12]);
    } else {
        r.fail(
            "aeneas require",
            &format!(
                "lakefile.lean pins another Aeneas; this h5i expects {}",
                pins::AENEAS_REV
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_commits_from_version_lines() {
        assert_eq!(
            commit_in("0.1.273 (4bd5a29f6e97ce2201ed35251afe5716e53b0a3a)").as_deref(),
            Some("4bd5a29f6e97ce2201ed35251afe5716e53b0a3a")
        );
        assert_eq!(commit_in("aeneas b86120db").as_deref(), Some("b86120db"));
        assert!(same_commit("b86120db", pins::AENEAS_REV));
        assert_eq!(commit_in("0.1.273"), None);
    }
}
