//! Persist the CLI's observations before temporary build directories disappear.
use crate::{manifest::Project, util::Out};
use anyhow::Result;
use h5i_app_report::{Recorder, Stage};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::Instant;

pub fn start(p: &Project, kind: &str, targets: Vec<String>) -> Result<Recorder> {
    let root = crate::util::git_toplevel(&p.root).unwrap_or_else(|| p.root.clone());
    let mut r = Recorder::start(&p.root, &root, kind, targets)?;
    for (name, value) in [
        (
            "lean-toolchain",
            std::fs::read_to_string(p.proofs().join("lean-toolchain"))
                .unwrap_or_else(|_| "unknown".into()),
        ),
        ("aeneas_expected", crate::pins::AENEAS_REV.into()),
        ("charon_expected", crate::pins::CHARON_REV.into()),
    ] {
        r.run.toolchain.insert(name.into(), value.trim().into());
    }
    for tool in ["lean", "cargo", "rustc"] {
        let value = std::process::Command::new(tool)
            .arg("--version")
            .current_dir(p.proofs())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|| "unavailable".into());
        r.run.toolchain.insert(tool.into(), value);
    }
    r.save()?;
    Ok(r)
}

pub fn tail(path: &Path) -> String {
    let Ok(mut f) = std::fs::File::open(path) else {
        return String::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(16_384);
    let _ = f.seek(SeekFrom::Start(start));
    let mut bytes = Vec::new();
    let _ = f.take(16_384).read_to_end(&mut bytes);
    let text = String::from_utf8_lossy(&bytes);
    if start > 0 {
        format!("[earlier output omitted]\n{text}")
    } else {
        text.into_owned()
    }
}

pub fn step<T>(
    r: &mut Recorder,
    name: &str,
    out: &Out,
    f: impl FnOnce(&Out) -> Result<T>,
) -> Result<T> {
    r.run.stages.push(Stage {
        name: name.into(),
        status: "running".into(),
        seconds: 0.0,
        log: String::new(),
    });
    r.save()?;
    let log = tempfile::NamedTempFile::new()?;
    let time = Instant::now();
    let result = f(&Out::Log(log.path().into()));
    let text = tail(log.path());
    out.note(&text);
    let stage = r.run.stages.last_mut().unwrap();
    stage.seconds = time.elapsed().as_secs_f64();
    stage.status = if result.is_ok() { "passed" } else { "failed" }.into();
    stage.log = text;
    if let Err(e) = &result {
        stage.log.push_str(&format!("\n{e:#}"));
    }
    r.save()?;
    result
}
