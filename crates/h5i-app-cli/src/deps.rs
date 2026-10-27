//! The framework an `[authz]` app links is the framework the proofs are about.
//!
//! `authz_kernel!`, `Authz<T>` and the generated store name `::h5i_app_core`,
//! `::h5i_app_std`, `::h5i_app_pg` and `::h5i_app_sql`, and those names are
//! resolved by each crate's own `Cargo.toml`. A `package = "…"` rename, a
//! `[patch]` or a path to a look-alike crate would put other code behind the
//! same names (an `Authz<T>` that skips the guard, a store that loads an empty
//! state) with the Rust and the extracted Lean unchanged. This reads Cargo's
//! own resolution (`cargo metadata`), not the source: every `h5i_app_*`
//! dependency of the project's crates must be the package of that name, and
//! all of them must come from the one place `h5i-app-core` comes from (the
//! same checkout, registry or git source). A fork of the whole framework is
//! out of scope, like a modified compiler.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::manifest::Project;
use crate::util;

/// Where a framework package comes from: its source, or for a path package
/// the directory that holds the framework's crates.
fn origin(p: &util::Package) -> String {
    match &p.source {
        Some(s) => s.split('#').next().unwrap_or(s).to_string(),
        None => p
            .manifest_path
            .parent()
            .and_then(Path::parent)
            .map(|d| d.display().to_string())
            .unwrap_or_default(),
    }
}

pub fn framework_problems(p: &Project) -> Result<Vec<String>> {
    if p.manifest.authz.guarded.is_empty() {
        return Ok(Vec::new());
    }
    let Some(krate) = p.krate() else { return Ok(Vec::new()) };
    let meta = util::metadata(&krate, true)?;
    let by_id: BTreeMap<&str, &util::Package> = meta.packages.iter().map(|q| (q.id.as_str(), q)).collect();
    let mut bad = Vec::new();
    let core: Vec<&&util::Package> = by_id.values().filter(|q| q.name == "h5i-app-core").collect();
    let want = match core.as_slice() {
        [one] => origin(one),
        [] => return Ok(vec!["`[authz]`: the kernel does not depend on `h5i-app-core`".into()]),
        many => {
            let all: Vec<String> = many.iter().map(|q| origin(q)).collect();
            return Ok(vec![format!("`[authz]`: more than one `h5i-app-core` in the build: {}", all.join(", "))]);
        }
    };
    let Some(resolve) = &meta.resolve else { return Ok(vec!["`cargo metadata` gave no resolution".into()]) };
    let root = p.root.canonicalize().unwrap_or(p.root.clone());
    for node in &resolve.nodes {
        let Some(pkg) = by_id.get(node.id.as_str()) else { continue };
        let mine = pkg.manifest_path.canonicalize().is_ok_and(|m| m.starts_with(&root));
        if !mine {
            continue;
        }
        for d in &node.deps {
            if !d.name.starts_with("h5i_app_") {
                continue;
            }
            let Some(dep) = by_id.get(d.pkg.as_str()) else { continue };
            if dep.name.replace('-', "_") != d.name {
                bad.push(format!(
                    "{}: `{}` is the package `{}` (a `package =` rename): with `[authz]` the framework's names must \
                     be the framework's packages",
                    pkg.name, d.name, dep.name
                ));
            } else if origin(dep) != want {
                bad.push(format!(
                    "{}: `{}` comes from {}, not from {} where `h5i-app-core` comes from (a `[patch]` or a path to \
                     another copy?)",
                    pkg.name,
                    dep.name,
                    origin(dep),
                    want
                ));
            }
        }
    }
    bad.sort();
    bad.dedup();
    Ok(bad)
}
