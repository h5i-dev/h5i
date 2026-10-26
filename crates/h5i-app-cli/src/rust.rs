//! The Rust a project builds, parsed with `syn`: the checks in `lint` and
//! `check` that ask what a server runs read it here, not as text.
//!
//! A crate's sources are its module tree: `src/lib.rs`, `src/main.rs` and
//! `src/bin/*.rs`, then every `mod` they declare, inline or in a file. Items
//! under `#[cfg(test)]` are left out, and so is any file no `mod` reaches,
//! so a test's impl or a stray file can neither satisfy nor fail a check. A
//! file that does not parse is an error, never skipped.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use syn::visit::Visit;

/// One `impl <Trait> for …` with its methods.
pub struct Impl {
    pub file: PathBuf,
    pub line: usize,
    pub fns: BTreeMap<String, syn::Block>,
}

/// The parsed module trees of every crate under a project root.
#[derive(Default)]
pub struct Sources {
    /// `(file, crate dir, items)` for each module file, `cfg(test)` removed.
    pub files: Vec<(PathBuf, PathBuf, Vec<syn::Item>)>,
    pub errors: Vec<String>,
}

fn is_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
        a.path().is_ident("cfg")
            && a.parse_args::<syn::Ident>().is_ok_and(|i| i == "test")
    })
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
        self.collect(krate, parsed.items, dir, &mut items, seen);
        self.files.push((file.to_path_buf(), krate.to_path_buf(), items));
    }

    /// Keep non-test items; flatten inline modules (their files go in `dir/name`)
    /// and follow `mod name;` to its file.
    fn collect(&mut self, krate: &Path, items: Vec<syn::Item>, dir: &Path, out: &mut Vec<syn::Item>, seen: &mut HashSet<PathBuf>) {
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
                            syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(s), .. }) => Some(dir.join(s.value())),
                            _ => None,
                        },
                        _ => None,
                    });
                    match m.content {
                        Some((_, inner)) => self.collect(krate, inner, &dir.join(&name), out, seen),
                        None => {
                            let candidates = match explicit {
                                Some(p) => vec![p],
                                None => vec![dir.join(format!("{name}.rs")), dir.join(&name).join("mod.rs")],
                            };
                            match candidates.iter().find(|c| c.is_file()) {
                                Some(f) => self.add_file(krate, f, &dir.join(&name), seen),
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
        for (_, _, items) in &self.files {
            for i in items {
                if let syn::Item::Use(u) = i {
                    walk(&u.tree, name, &mut out);
                }
            }
        }
        out
    }

    /// Every `impl <trait> for …`, the trait named by its last path segment
    /// (any spelling of the path) or a rename of it.
    pub fn impls_of(&self, trait_name: &str) -> Vec<Impl> {
        let names = self.names_of(trait_name);
        let mut out = Vec::new();
        for (file, _, items) in &self.files {
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
                        syn::ImplItem::Fn(f) => Some((f.sig.ident.to_string(), f.block.clone())),
                        _ => None,
                    })
                    .collect();
                out.push(Impl { file: file.clone(), line: imp.impl_token.span.start().line, fns });
            }
        }
        out
    }

    /// The body of a free function `name`, anywhere in the sources.
    pub fn free_fn(&self, name: &str) -> Option<&syn::Block> {
        self.files.iter().flat_map(|(_, _, items)| items).find_map(|i| match i {
            syn::Item::Fn(f) if f.sig.ident == name => Some(&*f.block),
            _ => None,
        })
    }

    /// Whether the sources define a free function `name` themselves (a
    /// macro's expansion is not visible here, so this is a hand-written one).
    pub fn defines_fn(&self, name: &str) -> bool {
        self.free_fn(name).is_some()
    }
}

/// `block` calls `name`: `name(…)`, `path::name(…)`, `path::name::<…>(…)` or
/// `x.name(…)`. Only real calls count, not a mention in a string or comment.
pub fn calls(block: &syn::Block, name: &str) -> bool {
    struct V<'a> {
        name: &'a str,
        hit: bool,
    }
    impl<'ast> Visit<'ast> for V<'_> {
        fn visit_expr_call(&mut self, c: &'ast syn::ExprCall) {
            if let syn::Expr::Path(p) = &*c.func
                && p.path.segments.last().is_some_and(|s| s.ident == self.name)
            {
                self.hit = true;
            }
            syn::visit::visit_expr_call(self, c);
        }
        fn visit_expr_method_call(&mut self, c: &'ast syn::ExprMethodCall) {
            if c.method == self.name {
                self.hit = true;
            }
            syn::visit::visit_expr_method_call(self, c);
        }
    }
    let mut v = V { name, hit: false };
    v.visit_block(block);
    v.hit
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
        let a = impls.iter().find(|i| i.fns.contains_key("transition")).unwrap();
        let b = impls.iter().find(|i| i.fns.contains_key("apply")).unwrap();
        assert_eq!((a.line, b.line), (6, 1));
        assert!(calls(&a.fns["transition"], "guarded") && !calls(&a.fns["transition"], "decoy"));
        assert!(calls(&b.fns["apply"], "apply"));
    }

    #[test]
    fn renamed_traits_and_unparsable_files() {
        let (_d, s) = tree(&[("src/main.rs", "use h5i_app_core::{Kernel as K};\nimpl K for A {}\n")]);
        assert_eq!(s.impls_of("Kernel").len(), 1);
        let (_d, s) = tree(&[("src/lib.rs", "mod gone;\nimpl Kernel for {\n")]);
        assert!(!s.errors.is_empty(), "a file that does not parse is an error, not skipped");
    }

    #[test]
    fn calls_are_calls() {
        let b: syn::Block = syn::parse_str("{ let x = \"apply(\"; reapply(s); s.apply_one(); k::apply::<T>(s) }").unwrap();
        assert!(calls(&b, "apply"));
        let b: syn::Block = syn::parse_str("{ let x = \"apply(\"; reapply(s); s.apply_one() }").unwrap();
        assert!(!calls(&b, "apply"), "a string, a longer name and another method do not count");
    }
}
