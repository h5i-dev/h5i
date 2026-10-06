//! `h5i app extract`: the kernel crate to Lean, Rust -> LLBC (Charon) -> Lean
//! (Aeneas), into `proofs/generated/`.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::manifest::Project;
use crate::util::{self, Out};

/// Charon start points for everything `schema!` generates in crate `k`, so
/// each lemma in the generated `Schema.lean` has its function extracted.
fn schema_items(k: &str) -> Vec<String> {
    let mut v = Vec::new();
    for f in ["apply", "sql_writes", "decode"] {
        v.extend(["--start-from-if-exists".into(), format!("{k}::{f}")]);
    }
    for m in [
        "put",
        "del",
        "del_where",
        "sql_put",
        "sql_del",
        "sql_del_where",
        "from_one",
        "from_rows",
    ] {
        v.extend(["--start-from-if-exists".into(), format!("{k}::_::{m}")]);
    }
    v
}

pub fn extract(p: &Project, out: &Out) -> Result<()> {
    extract_into(p, &p.generated(), true, out)
}

/// `extract` without rewriting the schema's Lean. For `mutate`, whose edits
/// never reach a `schema!` body, so the sandbox's copy is already current.
pub fn extract_kernel(p: &Project, out: &Out) -> Result<()> {
    extract_into(p, &p.generated(), false, out)
}

/// Extract to a scratch copy of `proofs/generated/` and fail if anything
/// differs from what is there: the committed Lean is exactly what Charon and
/// Aeneas make of the kernel, and the schema's Lean is what `schema!`
/// renders. Leaves the project as it found it, including the `Schema.lean`
/// that `schema!`'s test writes in place.
pub fn check_fresh(p: &Project, out: &Out) -> Result<()> {
    let real = p.generated();
    let before = snapshot(&real)?;
    let tmp = tempfile::tempdir()?;
    let scratch = tmp.path().join("generated");
    std::fs::create_dir_all(&scratch)?;
    for (name, bytes) in &before {
        std::fs::write(scratch.join(name), bytes)?;
    }
    let result = extract_into(p, &scratch, true, out);
    // The schema's test writes to its own path, in the real directory.
    let touched = snapshot(&real)?;
    for (name, bytes) in &before {
        if touched.get(name) != Some(bytes) {
            std::fs::write(real.join(name), bytes)?;
        }
    }
    for name in touched.keys().filter(|n| !before.contains_key(*n)) {
        std::fs::remove_file(real.join(name))?;
    }
    result?;
    let after = snapshot(&scratch)?;
    let mut stale: Vec<&String> = after.iter().filter(|(n, b)| before.get(*n) != Some(b)).map(|(n, _)| n).collect();
    stale.extend(touched.iter().filter(|(n, b)| before.get(*n) != Some(b)).map(|(n, _)| n));
    stale.sort();
    stale.dedup();
    if !stale.is_empty() {
        let names: Vec<String> = stale.iter().map(|n| n.to_string()).collect();
        bail!(
            "{}: proofs/generated/ is not what the kernel extracts to ({}); run `h5i app extract`",
            p.display(),
            names.join(", ")
        );
    }
    out.note(&format!("{}: extraction is up to date", p.display()));
    Ok(())
}

/// The `.lean` files of a directory, by name.
fn snapshot(dir: &Path) -> Result<std::collections::BTreeMap<String, Vec<u8>>> {
    let mut m = std::collections::BTreeMap::new();
    if dir.is_dir() {
        for f in lean_files(dir)? {
            m.insert(f.file_name().unwrap().to_string_lossy().into_owned(), std::fs::read(&f)?);
        }
    }
    Ok(m)
}

fn extract_into(p: &Project, dest: &Path, schema_lean: bool, out: &Out) -> Result<()> {
    let Some(ex) = &p.manifest.extract else {
        bail!(
            "{}: no [extract] section, so nothing to extract",
            p.display()
        );
    };
    let krate = p.root.join(&ex.krate);
    let meta = util::metadata(&krate, ex.std_specs)?;
    let pkg = meta.package_at(&krate)?;
    let id = util::ident(&pkg.name);

    let tmp = tempfile::tempdir()?;
    let llbc = tmp.path().join(format!("{id}.llbc"));
    let mut charon = Command::new("charon");
    charon
        .current_dir(&krate)
        .args(["cargo", "--preset=aeneas"]);
    for s in &ex.start_from {
        charon.args(["--start-from", &format!("{id}::{s}")]);
    }
    if ex.schema {
        charon.args(schema_items(&id));
    }
    for i in &ex.include {
        charon.args(["--include", i]);
    }
    charon.args(&ex.charon_args).arg("--dest-file").arg(&llbc);
    util::run(&mut charon, out)?;

    std::fs::create_dir_all(dest)?;
    util::run(
        Command::new("aeneas")
            .args(["-backend", "lean"])
            .arg(&llbc)
            .arg("-dest")
            .arg(dest),
        out,
    )?;

    let module = util::lean_module(&id);
    if !dest.join(format!("{module}.lean")).is_file() {
        bail!("Aeneas did not write {module}.lean in {}", dest.display());
    }
    let base = util::git_toplevel(&p.root).unwrap_or_else(|| p.root.clone());
    for f in lean_files(dest)? {
        let text = std::fs::read_to_string(&f)?;
        let norm = normalize_sources(&text, &meta.workspace_root, &base);
        if norm != text {
            std::fs::write(&f, norm)?;
        }
    }
    if ex.std_specs {
        std_specs(&meta, &module, &id, dest)?;
    }
    if ex.schema && schema_lean {
        bless_schema(&krate, out)?;
    }
    out.note(&format!(
        "extracted {} to {}",
        pkg.name,
        crate::manifest::display_path(&dest.join(format!("{module}.lean")))
    ));
    Ok(())
}

/// Write the Lean that `schema!` renders to the path its `lean "..."` names
/// (`Schema.lean` by convention). Only the kernel's own test knows that text,
/// so run it with `H5I_APP_BLESS=1`. Without a `lean` path the test does not
/// exist and nothing is written. Release, so a checkout that guards against
/// debug builds (CLAUDE.md) can run it.
fn bless_schema(krate: &Path, out: &Out) -> Result<()> {
    util::run(
        Command::new("cargo")
            .current_dir(krate)
            .env("H5I_APP_BLESS", "1")
            .args(["test", "--release", "--lib", "--", "h5i_app_lean_schema_is_current"]),
        out,
    )
    .context("rendering the schema's Lean (`schema!` with `lean \"...\"`)")
}

fn lean_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "lean"))
        .collect();
    v.sort();
    Ok(v)
}

/// Rewrite the `Source: '…'` paths Charon records to be relative to `base`
/// (the repository root), so the committed Lean does not depend on where the
/// checkout is. Charon writes them relative to the Cargo workspace root, or
/// absolute for crates outside it.
pub fn normalize_sources(text: &str, workspace_root: &Path, base: &Path) -> String {
    const TAG: &str = "Source: '";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find(TAG) {
        let (head, tail) = rest.split_at(i + TAG.len());
        out.push_str(head);
        let Some(end) = tail.find('\'') else {
            rest = tail;
            break;
        };
        out.push_str(&rebase(&tail[..end], workspace_root, base));
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

fn rebase(path: &str, workspace_root: &Path, base: &Path) -> String {
    let p = Path::new(path);
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        workspace_root.join(p)
    };
    let abs = clean(&abs);
    for b in [
        base.to_path_buf(),
        base.canonicalize().unwrap_or_else(|_| base.to_path_buf()),
    ] {
        if let Ok(r) = abs.strip_prefix(&b) {
            return r.to_string_lossy().into_owned();
        }
    }
    path.to_string()
}

/// Resolve `.` and `..` without touching the file system.
fn clean(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// Copy h5i-app-std's specs next to a kernel's extraction. The copy imports
/// the kernel's module and opens its namespace, where Aeneas puts the
/// `h5i_app_std.*` functions the kernel calls; their bodies are the crate's,
/// so the proofs go through unchanged.
fn std_specs(meta: &util::Metadata, module: &str, ns: &str, dest: &Path) -> Result<()> {
    let std = meta
        .packages
        .iter()
        .find(|p| p.name == "h5i-app-std")
        .context("`std-specs = true` but the crate does not depend on h5i-app-std")?;
    let src = std
        .manifest_path
        .parent()
        .unwrap()
        .join("proofs/StdSpecs.lean");
    let text = std::fs::read_to_string(&src).with_context(|| {
        format!(
            "reading {} (this h5i-app-std ships without its proofs)",
            src.display()
        )
    })?;
    let (mut import, mut open) = (false, false);
    let mut out = String::from(
        "-- Copied from h5i-app-std's proofs/StdSpecs.lean by `h5i app extract`. Do not edit.\n",
    );
    for line in text.lines() {
        if line == "import H5iAppStd" {
            import = true;
            out.push_str(&format!("import {module}\n"));
        } else if line == "open Aeneas Aeneas.Std Result H5iAppLib" {
            open = true;
            out.push_str(&format!("{line} {ns}\n"));
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    if !(import && open) {
        bail!(
            "{}: header changed; `h5i app extract` looks for `import H5iAppStd` and `open Aeneas Aeneas.Std Result H5iAppLib`",
            src.display()
        );
    }
    std::fs::write(dest.join("StdSpecs.lean"), out)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_become_relative_to_the_repository() {
        let ws = Path::new("/r/examples/app");
        let base = Path::new("/r");
        let text = "-- Source: 'atuin/kernel/src/lib.rs', lines 1:0-2:0\n-- Source: '/r/crates/s/src/lib.rs', lines 3\n-- Source: '/elsewhere/x.rs'";
        assert_eq!(
            normalize_sources(text, ws, base),
            "-- Source: 'examples/app/atuin/kernel/src/lib.rs', lines 1:0-2:0\n-- Source: 'crates/s/src/lib.rs', lines 3\n-- Source: '/elsewhere/x.rs'"
        );
    }

    #[test]
    fn sources_in_the_root_workspace_stay() {
        let r = Path::new("/r");
        assert_eq!(
            normalize_sources("Source: 'crates/a.rs'", r, r),
            "Source: 'crates/a.rs'"
        );
    }
}
