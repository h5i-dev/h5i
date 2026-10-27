//! The Rust a project builds, parsed with `syn`: the checks in `lint` and
//! `check` that ask what a server runs read it here, not as text.
//!
//! A crate's sources are its module tree: `src/lib.rs`, `src/main.rs` and
//! `src/bin/*.rs`, then every `mod` they declare, inline or in a file. Items
//! under `#[cfg(test)]` are left out, and so is any file no `mod` reaches,
//! so a test's impl or a stray file can neither satisfy nor fail a check. A
//! file that does not parse is an error, never skipped.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};


/// One `impl <Trait> for …` with its methods.
pub struct Impl {
    pub file: PathBuf,
    /// The crate directory it is in.
    pub krate: PathBuf,
    /// The last path segment of the type implemented for (`Authz` in `Authz<T>`).
    pub self_ty: String,
    pub line: usize,
    /// The methods it defines.
    pub fns: BTreeSet<String>,
}

/// The parsed module trees of every crate under a project root.
#[derive(Default)]
pub struct Sources {
    /// `(file, crate dir, items)` for each module file, `cfg(test)` removed.
    pub files: Vec<(PathBuf, PathBuf, Vec<syn::Item>)>,
    pub errors: Vec<String>,
}

/// `#[cfg(test)]` or `#[cfg(all(test, …))]`: compiled only for tests.
fn is_cfg_test(attrs: &[syn::Attribute]) -> bool {
    fn test_only(m: &syn::Meta) -> bool {
        match m {
            syn::Meta::Path(p) => p.is_ident("test"),
            syn::Meta::List(l) if l.path.is_ident("all") => l
                .parse_args_with(syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated)
                .is_ok_and(|ms| ms.iter().any(test_only)),
            _ => false,
        }
    }
    attrs.iter().any(|a| a.path().is_ident("cfg") && a.parse_args::<syn::Meta>().is_ok_and(|m| test_only(&m)))
}

fn has_cfg(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| a.path().is_ident("cfg"))
}

/// Crate directories under `root`: a `Cargo.toml` with a `src/`.
fn crates_under(root: &Path, out: &mut Vec<PathBuf>) {
    if root.join("Cargo.toml").is_file() && root.join("src").is_dir() {
        out.push(root.to_path_buf());
    }
    let Ok(entries) = std::fs::read_dir(root) else { return };
    let mut dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    dirs.sort();
    for d in dirs {
        let name = d.file_name().unwrap_or_default().to_string_lossy().into_owned();
        if !matches!(name.as_str(), "target" | ".lake" | ".git" | "proofs" | "tests" | "node_modules") {
            crates_under(&d, out);
        }
    }
}

impl Sources {
    /// Every crate under `root`.
    pub fn under(root: &Path) -> Sources {
        let mut crates = Vec::new();
        crates_under(root, &mut crates);
        let mut s = Sources::default();
        for k in crates {
            s.add_crate(&k);
        }
        s
    }

    /// One crate.
    pub fn of_crate(krate: &Path) -> Sources {
        let mut s = Sources::default();
        s.add_crate(krate);
        s
    }

    fn add_crate(&mut self, krate: &Path) {
        let src = krate.join("src");
        let mut roots: Vec<PathBuf> = ["lib.rs", "main.rs"].iter().map(|f| src.join(f)).filter(|f| f.is_file()).collect();
        // Targets `Cargo.toml` places elsewhere: `[lib] path`, `[[bin]] path`.
        let manifest = std::fs::read_to_string(krate.join("Cargo.toml"))
            .ok()
            .and_then(|t| t.parse::<toml::Table>().ok())
            .unwrap_or_default();
        let lib = manifest.get("lib").and_then(|l| l.get("path")).and_then(|p| p.as_str());
        if let Some(l) = lib {
            roots.retain(|r| !r.ends_with("lib.rs"));
            roots.push(krate.join(l));
        }
        for b in manifest.get("bin").and_then(|b| b.as_array()).into_iter().flatten() {
            if let Some(p) = b.get("path").and_then(|p| p.as_str()) {
                roots.push(krate.join(p));
            }
        }
        if let Ok(bins) = std::fs::read_dir(src.join("bin")) {
            let mut bins: Vec<PathBuf> = bins.flatten().map(|e| e.path()).collect();
            bins.sort();
            for b in bins {
                if b.extension().is_some_and(|x| x == "rs") {
                    roots.push(b);
                } else if b.join("main.rs").is_file() {
                    roots.push(b.join("main.rs"));
                }
            }
        }
        let mut seen = HashSet::new();
        for r in roots {
            if !r.is_file() {
                self.errors.push(format!("{}: target source is missing", r.display()));
                continue;
            }
            // A root's own `mod` files sit next to it.
            let dir = r.parent().unwrap_or(&src).to_path_buf();
            self.add_file(krate, &r, &dir, &mut seen);
        }
    }

    /// Parse `file`, whose child modules live in `dir`.
    fn add_file(&mut self, krate: &Path, file: &Path, dir: &Path, seen: &mut HashSet<PathBuf>) {
        if !seen.insert(file.to_path_buf()) {
            return;
        }
        let text = match std::fs::read_to_string(file) {
            Ok(t) => t,
            Err(e) => return self.errors.push(format!("{}: {e}", file.display())),
        };
        let parsed = match syn::parse_file(&text) {
            Ok(f) => f,
            Err(e) => {
                let l = e.span().start().line;
                return self.errors.push(format!("{}:{l}: does not parse: {e}", file.display()));
            }
        };
        let mut items = Vec::new();
        // `#[path]` on a module of this file is relative to the file's own
        // directory; its plain child modules live in `dir`.
        let base = file.parent().unwrap_or(dir).to_path_buf();
        self.collect(krate, parsed.items, dir, &base, &mut items, seen);
        self.files.push((file.to_path_buf(), krate.to_path_buf(), items));
    }

    /// Keep non-test items; flatten inline modules (their files go in
    /// `dir/name`) and follow `mod name;` to its file. `base` is what a
    /// `#[path]` here is relative to.
    #[allow(clippy::too_many_arguments)]
    fn collect(
        &mut self,
        krate: &Path,
        items: Vec<syn::Item>,
        dir: &Path,
        base: &Path,
        out: &mut Vec<syn::Item>,
        seen: &mut HashSet<PathBuf>,
    ) {
        for item in items {
            let attrs = match &item {
                syn::Item::Mod(m) => &m.attrs,
                syn::Item::Impl(i) => &i.attrs,
                syn::Item::Fn(f) => &f.attrs,
                syn::Item::Use(u) => &u.attrs,
                _ => {
                    out.push(item);
                    continue;
                }
            };
            if is_cfg_test(attrs) {
                continue;
            }
            match item {
                syn::Item::Mod(m) => {
                    let name = m.ident.to_string();
                    let explicit = m.attrs.iter().find(|a| a.path().is_ident("path")).and_then(|a| match &a.meta {
                        syn::Meta::NameValue(nv) => match &nv.value {
                            syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(s), .. }) => Some(base.join(s.value())),
                            _ => None,
                        },
                        _ => None,
                    });
                    let cfgd = has_cfg(&m.attrs);
                    match m.content {
                        Some((_, inner)) => {
                            let d = dir.join(&name);
                            self.collect(krate, inner, &d, &d, out, seen)
                        }
                        None => {
                            let path_attr = explicit.is_some();
                            let candidates = match explicit {
                                Some(p) => vec![p],
                                None => vec![dir.join(format!("{name}.rs")), dir.join(&name).join("mod.rs")],
                            };
                            match candidates.iter().find(|c| c.is_file()) {
                                // A `#[path]` file's or a `mod.rs`'s children sit next
                                // to it; a `name.rs`'s in `name/`.
                                Some(f) if path_attr || f.ends_with("mod.rs") => {
                                    let d = f.parent().unwrap_or(dir).to_path_buf();
                                    self.add_file(krate, f, &d, seen)
                                }
                                Some(f) => self.add_file(krate, f, &dir.join(&name), seen),
                                // A module under some other `cfg` may legitimately
                                // have no file in this configuration.
                                None if cfgd => {}
                                None => self.errors.push(format!(
                                    "{}: `mod {name};` has no file ({})",
                                    dir.display(),
                                    candidates.iter().map(|c| c.display().to_string()).collect::<Vec<_>>().join(" or ")
                                )),
                            }
                        }
                    }
                }
                other => out.push(other),
            }
        }
    }

    /// Names a trait goes by: its own, and any `use … as` rename of it.
    fn names_of(&self, name: &str) -> HashSet<String> {
        fn walk(t: &syn::UseTree, name: &str, out: &mut HashSet<String>) {
            match t {
                syn::UseTree::Path(p) => walk(&p.tree, name, out),
                syn::UseTree::Rename(r) if r.ident == name => {
                    out.insert(r.rename.to_string());
                }
                syn::UseTree::Group(g) => g.items.iter().for_each(|i| walk(i, name, out)),
                _ => {}
            }
        }
        let mut out = HashSet::from([name.to_string()]);
        // Follow chains (`use x::Kernel as K1; use self::K1 as K2;`).
        loop {
            let before = out.len();
            for n in out.clone() {
                for (_, _, items) in &self.files {
                    for i in items {
                        if let syn::Item::Use(u) = i {
                            walk(&u.tree, &n, &mut out);
                        }
                    }
                }
            }
            if out.len() == before {
                break out;
            }
        }
    }

    /// Every `impl <trait> for …`, the trait named by its last path segment
    /// (any spelling of the path) or a rename of it.
    pub fn impls_of(&self, trait_name: &str) -> Vec<Impl> {
        let names = self.names_of(trait_name);
        let mut out = Vec::new();
        for (file, krate, items) in &self.files {
            for i in items {
                let syn::Item::Impl(imp) = i else { continue };
                let Some((_, path, _)) = &imp.trait_ else { continue };
                let Some(last) = path.segments.last() else { continue };
                if !names.contains(&last.ident.to_string()) {
                    continue;
                }
                let fns = imp
                    .items
                    .iter()
                    .filter_map(|it| match it {
                        syn::ImplItem::Fn(f) => Some(f.sig.ident.to_string()),
                        _ => None,
                    })
                    .collect();
                let self_ty = match &*imp.self_ty {
                    syn::Type::Path(p) => p.path.segments.last().map(|s| s.ident.to_string()).unwrap_or_default(),
                    _ => String::new(),
                };
                out.push(Impl {
                    file: file.clone(),
                    krate: krate.clone(),
                    self_ty,
                    line: imp.impl_token.span.start().line,
                    fns,
                });
            }
        }
        out
    }

    /// Item-position invocations of the macro at exactly `path`
    /// (`h5i_app_std::authz_kernel!`, with or without a leading `::`), so a
    /// local macro of the same name does not count.
    pub fn macro_calls(&self, path: &[&str]) -> usize {
        self.files
            .iter()
            .flat_map(|(_, _, items)| items)
            .filter(|i| match i {
                syn::Item::Macro(m) => {
                    m.mac.path.segments.len() == path.len()
                        && m.mac.path.segments.iter().zip(path).all(|(s, want)| s.ident == want)
                }
                _ => false,
            })
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(files: &[(&str, &str)]) -> (tempfile::TempDir, Sources) {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("Cargo.toml"), "[package]\nname = \"s\"\n").unwrap();
        for (p, t) in files {
            let f = d.path().join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, t).unwrap();
        }
        let s = Sources::of_crate(d.path());
        (d, s)
    }

    #[test]
    fn impls_follow_the_module_tree_and_skip_tests() {
        let (_d, s) = tree(&[
            ("src/lib.rs", "#[cfg(test)]\nmod tests;\nmod engine;\n#[cfg(test)]\nmod t { impl Kernel for T { fn transition() { decoy() } } }\nimpl ::h5i_app_core::Kernel for A { fn transition() { k::guarded() } }\n"),
            ("src/engine.rs", "mod inner { impl Kernel<X> for B { fn apply() { k::apply(s) } } }\n"),
            ("src/tests.rs", "impl Kernel for Decoy {}\n"),
            ("src/stray.rs", "impl Kernel for Stray {}\n"),
        ]);
        assert!(s.errors.is_empty(), "{:?}", s.errors);
        let impls = s.impls_of("Kernel");
        assert_eq!(impls.len(), 2, "A and B only: not the test module, tests.rs or an unreachable file");
        let a = impls.iter().find(|i| i.fns.contains("transition")).unwrap();
        let b = impls.iter().find(|i| i.fns.contains("apply")).unwrap();
        assert_eq!((a.line, b.line), (6, 1));
        assert_eq!(a.self_ty, "A");
    }

    #[test]
    fn renamed_traits_and_unparsable_files() {
        let (_d, s) = tree(&[("src/main.rs", "use h5i_app_core::{Kernel as K};\nimpl K for A {}\n")]);
        assert_eq!(s.impls_of("Kernel").len(), 1);
        let (_d, s) = tree(&[("src/lib.rs", "mod gone;\nimpl Kernel for {\n")]);
        assert!(!s.errors.is_empty(), "a file that does not parse is an error, not skipped");
    }

    #[test]
    fn module_tree_follows_rustc() {
        // `#[path]` in a non-mod-rs file is relative to that file's directory,
        // and a `#[path]` file's children sit next to it; a cfg'd module may
        // have no file; `cfg(all(test, …))` is test code; `[lib] path` and
        // rename chains are followed.
        let (_d, s) = tree(&[
            ("src/lib.rs", "mod shell;\n#[cfg(feature = \"never\")]\nmod optional;\n#[cfg(all(test, unix))]\nmod t { impl Kernel for T {} }\n"),
            ("src/shell.rs", "#[path = \"impl/k.rs\"]\nmod kern;\n"),
            ("src/impl/k.rs", "mod inner;\nimpl Kernel for Real {}\n"),
            ("src/impl/inner.rs", "use h5i_app_core::Kernel as K1;\nuse self::K1 as K2;\nimpl K2 for Chained {}\n"),
            ("src/shell/impl/k.rs", "impl Kernel for Decoy {}\n"),
        ]);
        assert!(s.errors.is_empty(), "{:?}", s.errors);
        assert_eq!(s.impls_of("Kernel").len(), 2, "Real and Chained, not the decoy or the test impl");
        let (_d, s) = tree(&[
            ("Cargo.toml", "[package]\nname = \"s\"\n[lib]\npath = \"src/real.rs\"\n"),
            ("src/lib.rs", "impl Kernel for Decoy {}\n"),
            ("src/real.rs", "impl Kernel for Real {}\n"),
        ]);
        let impls = s.impls_of("Kernel");
        assert_eq!(impls.len(), 1);
        assert!(impls[0].file.ends_with("real.rs"));
    }

}
