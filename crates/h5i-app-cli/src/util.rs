//! Subprocesses and Cargo metadata.

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// Where a subprocess's output goes.
#[derive(Clone, Debug)]
pub enum Out {
    /// The terminal.
    Inherit,
    /// Appended to a log file (mutants run in parallel and keep a log each).
    Log(PathBuf),
}

impl Out {
    pub fn note(&self, msg: &str) {
        match self {
            Out::Inherit => eprintln!("{msg}"),
            Out::Log(p) => {
                if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(p) {
                    use std::io::Write;
                    let _ = writeln!(f, "{msg}");
                }
            }
        }
    }
}

/// Run `cmd`; succeed only on exit status 0.
pub fn run(cmd: &mut Command, out: &Out) -> Result<()> {
    if status(cmd, out)? {
        Ok(())
    } else {
        bail!("`{}` failed", show(cmd))
    }
}

/// Run `cmd` and report whether it exited 0. Errors only if it cannot start.
pub fn status(cmd: &mut Command, out: &Out) -> Result<bool> {
    out.note(&format!("$ {}", show(cmd)));
    cmd.stdin(Stdio::null());
    if let Out::Log(p) = out {
        let f = OpenOptions::new().create(true).append(true).open(p)?;
        cmd.stdout(f.try_clone()?).stderr(f);
    }
    let prog = cmd.get_program().to_string_lossy().to_string();
    let st = cmd.status().with_context(|| {
        format!("cannot run `{prog}`; is it installed and on PATH? (`h5i app doctor`)")
    })?;
    Ok(st.success())
}

/// Run `cmd` and capture stdout, failing with its stderr.
pub fn capture(cmd: &mut Command) -> Result<String> {
    let prog = cmd.get_program().to_string_lossy().to_string();
    let o = cmd.stdin(Stdio::null()).output().with_context(|| {
        format!("cannot run `{prog}`; is it installed and on PATH? (`h5i app doctor`)")
    })?;
    if !o.status.success() {
        bail!(
            "`{}` failed:\n{}",
            show(cmd),
            String::from_utf8_lossy(&o.stderr).trim_end()
        );
    }
    Ok(String::from_utf8_lossy(&o.stdout).into_owned())
}

pub fn show(cmd: &Command) -> String {
    let mut s = cmd.get_program().to_string_lossy().to_string();
    for a in cmd.get_args() {
        let a = a.to_string_lossy();
        if a.contains(' ') {
            s.push_str(&format!(" '{a}'"))
        } else {
            s.push_str(&format!(" {a}"))
        }
    }
    s
}

/// True if `prog` is an executable on PATH.
pub fn on_path(prog: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|d| d.join(prog))
        .find(|p| p.is_file())
}

#[derive(Debug, Deserialize)]
pub struct Metadata {
    pub packages: Vec<Package>,
    pub workspace_root: PathBuf,
    pub resolve: Option<Resolve>,
}

#[derive(Debug, Deserialize)]
pub struct Package {
    pub name: String,
    pub id: String,
    pub manifest_path: PathBuf,
    pub source: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Resolve {
    pub nodes: Vec<Node>,
}

#[derive(Debug, Deserialize)]
pub struct Node {
    pub id: String,
    pub deps: Vec<NodeDep>,
}

#[derive(Debug, Deserialize)]
pub struct NodeDep {
    pub pkg: String,
}

/// `cargo metadata` for the crate at `dir`.
pub fn metadata(dir: &Path, deps: bool) -> Result<Metadata> {
    let mut cmd = Command::new("cargo");
    cmd.args(["metadata", "--format-version", "1"])
        .current_dir(dir);
    if !deps {
        cmd.arg("--no-deps");
    }
    let text = capture(&mut cmd)?;
    serde_json::from_str(&text).context("parsing `cargo metadata`")
}

impl Metadata {
    /// The package whose manifest is `dir/Cargo.toml`.
    pub fn package_at(&self, dir: &Path) -> Result<&Package> {
        let want = dir
            .join("Cargo.toml")
            .canonicalize()
            .with_context(|| format!("{} has no Cargo.toml", dir.display()))?;
        self.packages
            .iter()
            .find(|p| p.manifest_path.canonicalize().is_ok_and(|m| m == want))
            .with_context(|| format!("no package at {}", dir.display()))
    }

    /// The path packages `root` depends on, transitively, `root` included.
    pub fn local_closure<'a>(&'a self, root: &'a Package) -> Vec<&'a Package> {
        let Some(resolve) = &self.resolve else {
            return vec![root];
        };
        let mut seen = std::collections::HashSet::new();
        let mut stack = vec![root.id.as_str()];
        while let Some(id) = stack.pop() {
            if !seen.insert(id) {
                continue;
            }
            if let Some(n) = resolve.nodes.iter().find(|n| n.id == id) {
                stack.extend(n.deps.iter().map(|d| d.pkg.as_str()));
            }
        }
        self.packages
            .iter()
            .filter(|p| p.source.is_none() && seen.contains(p.id.as_str()))
            .collect()
    }
}

/// The root of the git repository containing `dir`, if any.
pub fn git_toplevel(dir: &Path) -> Option<PathBuf> {
    let out = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(dir)
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
}

/// Copy a directory tree, skipping build output and VCS metadata.
pub fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from).with_context(|| format!("reading {}", from.display()))? {
        let e = e?;
        let name = e.file_name();
        if matches!(name.to_string_lossy().as_ref(), "target" | ".lake" | ".git") {
            continue;
        }
        let (src, dst) = (e.path(), to.join(&name));
        let ty = e.file_type()?;
        if ty.is_dir() {
            copy_tree(&src, &dst)?;
        } else if ty.is_symlink() && cfg!(unix) {
            #[cfg(unix)]
            std::os::unix::fs::symlink(std::fs::read_link(&src)?, &dst)?;
        } else if ty.is_symlink() && src.is_dir() {
            copy_tree(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst).with_context(|| format!("copying {}", src.display()))?;
        }
    }
    Ok(())
}

#[cfg(unix)]
pub fn symlink(from: &Path, to: &Path) -> Result<()> {
    Ok(std::os::unix::fs::symlink(from, to)?)
}

#[cfg(not(unix))]
pub fn symlink(from: &Path, to: &Path) -> Result<()> {
    Ok(std::os::windows::fs::symlink_dir(from, to)?)
}

/// The Rust crate identifier for a package name.
pub fn ident(package: &str) -> String {
    package.replace('-', "_")
}

/// The Lean module Aeneas names after a crate: `docs_kernel` is `DocsKernel`.
pub fn lean_module(ident: &str) -> String {
    ident
        .split('_')
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_names_follow_aeneas() {
        assert_eq!(lean_module("docs_kernel"), "DocsKernel");
        assert_eq!(lean_module("h5i_app_sql"), "H5iAppSql");
    }
}
