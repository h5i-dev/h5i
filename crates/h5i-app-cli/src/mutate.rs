//! `h5i app mutate`: put a bug in the kernel, re-extract it, rebuild the
//! proofs. A mutant is caught when the proofs stop building; one that survives
//! is a behaviour the spec does not pin down.
//!
//! Each mutant runs in its own copy of the project: the kernel and the path
//! crates it depends on, laid out as in the original so relative paths still
//! resolve, under a fresh Cargo workspace. The copy's Lake project shares the
//! original's packages (read-only), so a mutant rebuilds only the kernel's
//! extraction and the proofs above it.
//!
//! Two caches make that cheap. The copy starts from the original's Lake build
//! (`proofs/.lake/build`, copied, not linked, since a mutant overwrites it):
//! Lake rebuilds a module only when its inputs' hashes change, so hand-written
//! modules the re-extraction does not touch are not rebuilt. And each job
//! slot keeps one Cargo target directory across mutants and runs (under the
//! original's `target/h5i-app-mutate/`), so registry dependencies compile once.
//! A mutant holds its slot's lock for its whole run, and its copied crates'
//! fingerprints are deleted first, so they are always rebuilt: a previous
//! mutant's build (this run's or a concurrent one's) is never taken for this
//! one's, whatever the timestamps say.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use quote::ToTokens;
use syn::visit::Visit;

use crate::manifest::{Edit, Project};
use crate::util::{self, Out};

#[derive(Clone, Debug)]
pub struct Mutant {
    pub name: String,
    /// What the change is, for generated mutants: "`a <= b` -> `a < b`".
    pub what: Option<String>,
    /// Relative to the project root.
    pub file: PathBuf,
    pub change: Change,
}

#[derive(Clone, Debug)]
pub enum Change {
    /// Declared in the manifest: each `old` occurs exactly once, in the file
    /// or, given `function`, in that function's source.
    Replace { edits: Vec<Edit>, function: Option<String> },
    /// Generated: replace bytes `start..end`, which must read `was`.
    Splice {
        start: usize,
        end: usize,
        was: String,
        new: String,
    },
    /// Not run: the change would be to code no proof is about (a server's
    /// own principal function), so it survives by design. `why` says so.
    NotRun { why: String },
}

impl Mutant {
    pub fn apply(&self, src: &str) -> Result<String> {
        match &self.change {
            Change::Replace { edits, function } => {
                let mut s = src.to_string();
                for e in edits {
                    // Re-located per edit: an earlier edit moves the bytes.
                    let (start, end) = match function {
                        Some(f) => function_span(&s, f)?,
                        None => (0, s.len()),
                    };
                    let scope = &s[start..end];
                    let n = scope.matches(&e.old).count();
                    if n != 1 {
                        let within = function.as_ref().map_or(String::new(), |f| format!(" in `{f}`"));
                        bail!("pattern occurs {n} times{within}, not once: {:?}", trunc(&e.old));
                    }
                    let at = start + scope.find(&e.old).unwrap();
                    s = format!("{}{}{}", &s[..at], e.new, &s[at + e.old.len()..]);
                }
                Ok(s)
            }
            Change::Splice {
                start,
                end,
                was,
                new,
            } => {
                if src.get(*start..*end) != Some(was.as_str()) {
                    bail!("source changed under the mutant");
                }
                Ok(format!("{}{}{}", &src[..*start], new, &src[*end..]))
            }
            Change::NotRun { why } => bail!("not run: {why}"),
        }
    }
}

/// The byte range of function `name` in `src`, from `fn` to its closing
/// brace: a free function or method called `name`, or with `Type::name` a
/// method in an `impl` for `Type`. Test code counts too: a mutant names its
/// function on purpose.
fn function_span(src: &str, name: &str) -> Result<(usize, usize)> {
    let ast = syn::parse_file(src).context("parsing the file to find the mutant's function")?;
    let (ty, fun) = match name.rsplit_once("::") {
        Some((t, f)) => (Some(t.rsplit("::").next().unwrap_or(t)), f),
        None => (None, name),
    };
    let mut v = FnSpans { src, lines: line_starts(src), self_ty: None, found: Vec::new() };
    v.visit_file(&ast);
    let hits: Vec<_> = v
        .found
        .into_iter()
        .filter(|(t, f, _, _)| f == fun && ty.is_none_or(|ty| t.as_deref() == Some(ty)))
        .collect();
    match hits.as_slice() {
        [(_, _, s, e)] => Ok((*s, *e)),
        [] => bail!("no function `{name}` in the file"),
        _ => bail!("{} functions named `{name}` in the file; name one as `Type::{fun}`", hits.len()),
    }
}

/// Every function's span: (impl's self type, name, start, end).
struct FnSpans<'a> {
    src: &'a str,
    lines: Vec<usize>,
    self_ty: Option<String>,
    found: Vec<(Option<String>, String, usize, usize)>,
}

impl FnSpans<'_> {
    fn add(&mut self, name: &syn::Ident, from: &impl ToTokens, to: &impl ToTokens) {
        let at = |t: &dyn Fn() -> proc_macro2::TokenStream, last: bool| {
            let ts: Vec<_> = t().into_iter().collect();
            let tok = if last { ts.last() } else { ts.first() }?;
            let lc = if last { tok.span().end() } else { tok.span().start() };
            let start = *self.lines.get(lc.line.checked_sub(1)?)?;
            let line = &self.src[start..];
            Some(start + line.char_indices().nth(lc.column).map_or(line.len(), |(i, _)| i))
        };
        if let (Some(s), Some(e)) = (at(&|| from.to_token_stream(), false), at(&|| to.to_token_stream(), true)) {
            self.found.push((self.self_ty.clone(), name.to_string(), s, e));
        }
    }
}

impl<'ast> Visit<'ast> for FnSpans<'_> {
    fn visit_item_impl(&mut self, i: &'ast syn::ItemImpl) {
        let ty = match &*i.self_ty {
            syn::Type::Path(p) => p.path.segments.last().map(|s| s.ident.to_string()),
            _ => None,
        };
        let saved = std::mem::replace(&mut self.self_ty, ty);
        syn::visit::visit_item_impl(self, i);
        self.self_ty = saved;
    }

    fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
        // A nested function is no method of the enclosing impl.
        let saved = self.self_ty.take();
        self.add(&f.sig.ident, &f.sig, &f.block);
        syn::visit::visit_item_fn(self, f);
        self.self_ty = saved;
    }

    fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
        self.add(&f.sig.ident, &f.sig, &f.block);
        syn::visit::visit_impl_item_fn(self, f);
    }
}

fn trunc(s: &str) -> String {
    if s.chars().count() > 60 {
        s.chars().take(60).collect::<String>() + "…"
    } else {
        s.to_string()
    }
}

/// The manifest's `[[mutant]]`s.
pub fn declared(p: &Project) -> Result<Vec<Mutant>> {
    let default_file = p
        .manifest
        .extract
        .as_ref()
        .map(|e| PathBuf::from(&e.krate).join("src/lib.rs"));
    p.manifest
        .mutants
        .iter()
        .map(|m| {
            let file = match (&m.file, &default_file) {
                (Some(f), _) => PathBuf::from(f),
                (None, Some(d)) => d.clone(),
                (None, None) => bail!(
                    "mutant `{}` needs `file`: the project extracts no crate",
                    m.name
                ),
            };
            Ok(Mutant {
                name: m.name.clone(),
                what: None,
                file,
                change: Change::Replace { edits: m.edits()?, function: m.function.clone() },
            })
        })
        .collect()
}

/// Mutants generated from the kernel's syntax: comparison boundaries
/// (`<` and `<=`), equality flipped, `&&` and `||` swapped, a `!` dropped, an
/// `if` condition forced to `true` or `false`. Test code is skipped.
pub fn generated(p: &Project) -> Result<Vec<Mutant>> {
    let Some(krate) = p.krate() else {
        bail!("{}: no [extract] crate to mutate", p.display())
    };
    let mut files = Vec::new();
    rs_files(&krate.join("src"), &mut files);
    files.sort();
    let mut out = Vec::new();
    for f in files {
        let src = std::fs::read_to_string(&f)?;
        let ast = syn::parse_file(&src).with_context(|| format!("parsing {}", f.display()))?;
        let rel = f.strip_prefix(&p.root)?.to_path_buf();
        let mut v = Finder {
            src: &src,
            lines: line_starts(&src),
            func: String::new(),
            found: Vec::new(),
        };
        v.visit_file(&ast);
        let short = f.strip_prefix(&krate).unwrap_or(&f).display().to_string();
        let mut names = std::collections::HashMap::new();
        for (fun, line, start, end, was, new) in v.found {
            let kind = kind(&was, &new);
            let base = format!("{fun}@{short}:{line}:{kind}");
            let n = names.entry(base.clone()).or_insert(0);
            *n += 1;
            let name = if *n == 1 { base } else { format!("{base}#{n}") };
            let what = if new.is_empty() {
                format!("drop `{was}`")
            } else {
                format!("`{was}` -> `{new}`")
            };
            out.push(Mutant {
                name,
                what: Some(what),
                file: rel.clone(),
                change: Change::Splice {
                    start,
                    end,
                    was,
                    new,
                },
            });
        }
    }
    Ok(out)
}

/// A short tag for a generated change, used in its name.
fn kind(was: &str, new: &str) -> &'static str {
    match (was, new) {
        (_, "true") => "if-true",
        (_, "false") => "if-false",
        ("!", "") => "drop-not",
        ("<" | "<=" | ">" | ">=", _) => "boundary",
        ("==" | "!=", _) => "negate",
        ("&&" | "||", _) => "and-or",
        _ => "edit",
    }
}

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

fn line_starts(src: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(src.match_indices('\n').map(|(i, _)| i + 1))
        .collect()
}

struct Finder<'a> {
    src: &'a str,
    lines: Vec<usize>,
    func: String,
    /// (function, line, start, end, was, new)
    found: Vec<(String, usize, usize, usize, String, String)>,
}

impl Finder<'_> {
    /// Byte offset of a proc-macro2 location (1-based line, column in chars).
    fn offset(&self, lc: proc_macro2::LineColumn) -> Option<usize> {
        let start = *self.lines.get(lc.line.checked_sub(1)?)?;
        let line = &self.src[start..];
        let col = line
            .char_indices()
            .nth(lc.column)
            .map(|(i, _)| i)
            .unwrap_or(line.len());
        Some(start + col)
    }

    fn range(&self, t: &impl ToTokens) -> Option<(usize, usize, usize)> {
        let ts: Vec<_> = t.to_token_stream().into_iter().collect();
        let (first, last) = (ts.first()?, ts.last()?);
        let (s, e) = (first.span().start(), last.span().end());
        Some((s.line, self.offset(s)?, self.offset(e)?))
    }

    fn push(&mut self, t: &impl ToTokens, new: &str) {
        let Some((line, start, end)) = self.range(t) else {
            return;
        };
        let was = self.src[start..end].to_string();
        self.found
            .push((self.func.clone(), line, start, end, was, new.to_string()));
    }
}

fn is_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
        let s = a.to_token_stream().to_string().replace(' ', "");
        s == "#[test]" || s.starts_with("#[cfg(test") || s.contains("cfg(test)")
    })
}

impl<'ast> Visit<'ast> for Finder<'_> {
    fn visit_item_mod(&mut self, m: &'ast syn::ItemMod) {
        if !is_test(&m.attrs) {
            syn::visit::visit_item_mod(self, m);
        }
    }

    fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
        if is_test(&f.attrs) {
            return;
        }
        let saved = std::mem::replace(&mut self.func, f.sig.ident.to_string());
        syn::visit::visit_item_fn(self, f);
        self.func = saved;
    }

    fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
        if is_test(&f.attrs) {
            return;
        }
        let saved = std::mem::replace(&mut self.func, f.sig.ident.to_string());
        syn::visit::visit_impl_item_fn(self, f);
        self.func = saved;
    }

    fn visit_expr_binary(&mut self, e: &'ast syn::ExprBinary) {
        use syn::BinOp::*;
        let new = match e.op {
            Lt(_) => Some("<="),
            Le(_) => Some("<"),
            Gt(_) => Some(">="),
            Ge(_) => Some(">"),
            Eq(_) => Some("!="),
            Ne(_) => Some("=="),
            And(_) => Some("||"),
            Or(_) => Some("&&"),
            _ => None,
        };
        if let (Some(new), false) = (new, self.func.is_empty()) {
            self.push(&e.op, new);
        }
        syn::visit::visit_expr_binary(self, e);
    }

    fn visit_expr_unary(&mut self, e: &'ast syn::ExprUnary) {
        if matches!(e.op, syn::UnOp::Not(_)) && !self.func.is_empty() {
            // `!x` becomes `x`: splice the operator out.
            if let Some((line, start, _)) = self.range(&e.op) {
                self.found.push((
                    self.func.clone(),
                    line,
                    start,
                    start + 1,
                    "!".into(),
                    String::new(),
                ));
            }
        }
        syn::visit::visit_expr_unary(self, e);
    }

    fn visit_expr_if(&mut self, e: &'ast syn::ExprIf) {
        if !matches!(*e.cond, syn::Expr::Let(_)) && !self.func.is_empty() {
            let cond = &*e.cond;
            if !matches!(cond, syn::Expr::Lit(_)) {
                self.push(cond, "true");
                self.push(cond, "false");
            }
        }
        syn::visit::visit_expr_if(self, e);
    }

    // Macro bodies are tokens, not syntax: `schema!` and friends are not
    // mutated.
}

/// Security-oriented mutants (`--security`): each breaks an authorization
/// property, and is caught only if some spec the project proves depends on
/// it. A survivor is a property no spec pins down.
///
/// - In a hand-written `impl Policy`: drop a conjunct of `permits` (each
///   operand of each `&&`), make `visible` or `stored` return `true`, replace
///   a comparison with `actor` in `permits` or `visible` by `true`.
/// - In a policy from `schema!`'s `policy` block: the same changes made to the
///   generator (`h5i-app-schema/src/policy.rs`, copied into the sandbox).
/// - For `[authz]`: `guarded_transition` without the guard, and the guard
///   without its write check (`h5i-app-std/src/authz.rs`); the tenant of every
///   principal set to 0 in `authz_kernel!`.
/// - The principal function the server gives `HmacAuth::new` puts every
///   token in tenant 0 ([`PRINCIPAL_MUTANT`]): run on the kernel's function
///   when the server is linked to it, else listed as surviving by design.
pub fn security(p: &Project) -> Result<Vec<Mutant>> {
    let Some(krate) = p.krate() else {
        bail!("{}: no [extract] crate to mutate", p.display())
    };
    let mut out = Vec::new();
    // The crate's module files, test modules left out.
    let mut files: Vec<PathBuf> = crate::rust::Sources::of_crate(&krate).files.into_iter().map(|(f, _, _)| f).collect();
    files.sort();
    files.dedup();
    let mut generated_policy = false;
    for f in &files {
        let src = std::fs::read_to_string(f)?;
        generated_policy |= src.contains("schema!") && src.contains("\n    policy ");
        let ast = syn::parse_file(&src).with_context(|| format!("parsing {}", f.display()))?;
        let rel = f.strip_prefix(&p.root)?.to_path_buf();
        let mut v = PolicyFinder { src: &src, lines: line_starts(&src), fun: None, found: Vec::new() };
        v.visit_file(&ast);
        let short = f.strip_prefix(&krate).unwrap_or(f).display().to_string();
        let mut names = std::collections::HashMap::new();
        for (fun, kind, line, start, end, was, new) in v.found {
            let base = format!("security:{fun}@{short}:{line}:{kind}");
            let n = names.entry(base.clone()).or_insert(0);
            *n += 1;
            let name = if *n == 1 { base } else { format!("{base}#{n}") };
            out.push(Mutant {
                name,
                what: Some(format!("`{}` -> `{new}`", trunc(&was))),
                file: rel.clone(),
                change: Change::Splice { start, end, was, new },
            });
        }
        if !p.manifest.authz.guarded.is_empty() && src.contains("authz_kernel!") {
            for (old, new) in [("tenant: |actor| actor.org", "tenant: |actor| 0"), ("tenant: |actor| actor.tenant", "tenant: |actor| 0")] {
                if src.matches(old).count() == 1 {
                    out.push(declared_edit("security:tenant-zero", "every principal in tenant 0", rel.clone(), old, new));
                }
            }
        }
    }
    if let Some(m) = principal_mutant(p, &files)? {
        out.push(m);
    }
    let meta = util::metadata(&krate, true)?;
    let framework = |pkg: &str, file: &str| -> Option<PathBuf> {
        let q = meta.packages.iter().find(|q| q.name == pkg)?;
        let path = q.manifest_path.parent()?.join(file);
        relative(&p.root, &path)
    };
    if generated_policy && let Some(file) = framework("h5i-app-schema", "src/policy.rs") {
        for (name, what, old, new) in [
            (
                "security:policy-owner-may-change",
                "a replacement no longer keeps the owner",
                "Some(old) => $crate::__policy_keep!($owg x old) && (",
                "Some(old) => (",
            ),
            (
                "security:policy-create-as-anyone",
                "`owner` lets anyone create a row",
                "    (owner [$ow:ident] $a:ident $s:ident $x:ident $uf:ident $tf:ident) => {\n        $x.$ow == $a.$uf\n    };",
                "    (owner [$ow:ident] $a:ident $s:ident $x:ident $uf:ident $tf:ident) => {\n        true\n    };",
            ),
            (
                "security:policy-modify-anyones",
                "`owner` lets anyone replace or delete a row",
                "    (owner [$ow:ident] $a:ident $s:ident $old:ident $uf:ident) => {\n        $old.$ow == $a.$uf\n    };",
                "    (owner [$ow:ident] $a:ident $s:ident $old:ident $uf:ident) => {\n        true\n    };",
            ),
            (
                "security:policy-visible-true",
                "every row visible",
                "$( $crate::__policy_visible!($va $owg actor s x $uf) )||+",
                "true",
            ),
            ("security:policy-stored-true", "`stored` accepts any row with a stored key", "Some(r) => r == *x,", "Some(r) => true,"),
            ("security:policy-everyone-admin", "every member is an admin", "Some(r) => r.$af,", "Some(r) => true,"),
        ] {
            out.push(declared_edit(name, what, file.clone(), old, new));
        }
    }
    if !p.manifest.authz.guarded.is_empty()
        && let Some(file) = framework("h5i-app-std", "src/authz.rs")
    {
        out.push(declared_edit(
            "security:no-guard",
            "`guarded_transition` runs the kernel without the guard",
            file.clone(),
            "$crate::authz::guard_viewed::<$p, $s, $c, $w, $r, $e, $d, $app, $app, $app>(actor, s, cmd)",
            "match <$app as $crate::authz::Transition<$p, $s, $c, $w, $r, $e>>::kernel_transition(actor, s, cmd) {\n                ::core::result::Result::Ok(o) => ::core::result::Result::Ok(o),\n                ::core::result::Result::Err(e) => ::core::result::Result::Err($crate::authz::Guarded::App(e)),\n            }",
        ));
        out.push(declared_edit(
            "security:guard-skips-writes",
            "the guard no longer checks writes",
            file,
            "if !check_writes::<A, S, C, W, R, E, D, T, P>(actor, s, &out.0) {",
            "if false {",
        ));
    }
    Ok(out)
}

/// The name of the server-side principal mutant.
pub const PRINCIPAL_MUTANT: &str = "security:principal-tenant-zero";

/// The function the server gives `HmacAuth::new` puts every token in tenant
/// 0. When the server is linked (`crate::tenant`) that function is the
/// kernel's `[obligations] principal`, and the mutant shadows its first
/// argument (the tenant) with 0 there: `tenant-of-token` must break. When it
/// is not linked, the function is the server's own, which no proof is about:
/// the mutant is listed as surviving by design, with the reason, and not run.
/// None for a project with no authenticator to speak of.
fn principal_mutant(p: &Project, files: &[PathBuf]) -> Result<Option<Mutant>> {
    let l = crate::tenant::linkage(p);
    let opted = p.principal_fn().is_some();
    if !opted && l.constructions.is_empty() && l.custom.is_empty() {
        return Ok(None);
    }
    let what = "the principal function the server gives `HmacAuth::new` puts every token in tenant 0";
    let not_run = |why: String| Mutant {
        name: PRINCIPAL_MUTANT.into(),
        what: Some(what.into()),
        file: PathBuf::new(),
        change: Change::NotRun { why },
    };
    let Some(why_linked) = l.why_not() else {
        // Linked: the kernel's function, found by its last path segment.
        let f = p.principal_fn().unwrap_or_default();
        let name = f.rsplit("::").next().unwrap_or(f);
        for file in files {
            let src = std::fs::read_to_string(file)?;
            let ast = syn::parse_file(&src).with_context(|| format!("parsing {}", file.display()))?;
            let Some(func) = ast.items.iter().find_map(|i| match i {
                syn::Item::Fn(x) if x.sig.ident == name => Some(x),
                _ => None,
            }) else {
                continue;
            };
            let first = func.sig.inputs.first().and_then(|a| match a {
                syn::FnArg::Typed(t) => match &*t.pat {
                    syn::Pat::Ident(i) => Some(i.ident.to_string()),
                    _ => None,
                },
                _ => None,
            });
            let Some(tenant) = first else {
                return Ok(Some(not_run(format!("`{f}`'s first argument is not a plain name, so the mutant cannot shadow it"))));
            };
            let finder = Finder { src: &src, lines: line_starts(&src), func: String::new(), found: Vec::new() };
            let Some(at) = finder.offset(func.block.brace_token.span.open().end()) else { continue };
            // `p.root` is canonical (on macOS the temp dir `/var` is `/private/var`).
            let canon = file.canonicalize().unwrap_or_else(|_| file.clone());
            let rel = canon.strip_prefix(&p.root).unwrap_or(file).to_path_buf();
            return Ok(Some(Mutant {
                name: PRINCIPAL_MUTANT.into(),
                what: Some(format!("{what} (linked: the kernel's `{f}`, its `{tenant}` shadowed by 0)")),
                file: rel,
                change: Change::Splice { start: at, end: at, was: String::new(), new: format!("\n    let {tenant}: u64 = 0;") },
            }));
        }
        return Ok(Some(not_run(format!("linked, but `{f}` was not found at the top level of a kernel source file"))));
    };
    let why = if opted {
        format!("no proof is about the function the server uses: {why_linked}")
    } else {
        let used: Vec<String> = l
            .constructions
            .iter()
            .map(|c| format!("{} passes `{}`", c.at, c.what))
            .chain(l.custom.iter().map(|c| format!("{c}: impl Authenticator")))
            .collect();
        format!(
            "`tenant-of-token` is not stated (no `[obligations] principal`), so no proof is about the principal function \
             the server uses ({})",
            used.join("; ")
        )
    };
    Ok(Some(not_run(why)))
}

fn declared_edit(name: &str, what: &str, file: PathBuf, old: &str, new: &str) -> Mutant {
    Mutant {
        name: name.into(),
        what: Some(what.into()),
        file,
        change: Change::Replace { edits: vec![Edit { old: old.into(), new: new.into() }], function: None },
    }
}

/// `to` relative to `from`, through `..` where needed.
fn relative(from: &Path, to: &Path) -> Option<PathBuf> {
    let from = from.canonicalize().ok()?;
    let to = to.canonicalize().ok()?;
    let common = from.ancestors().find(|a| to.starts_with(a))?;
    let mut out = PathBuf::new();
    for _ in from.strip_prefix(common).ok()?.components() {
        out.push("..");
    }
    out.push(to.strip_prefix(common).ok()?);
    Some(out)
}

/// Finds the security mutants of a hand-written `impl Policy`.
struct PolicyFinder<'a> {
    src: &'a str,
    lines: Vec<usize>,
    /// The `Policy` method being visited.
    fun: Option<String>,
    /// (function, kind, line, start, end, was, new)
    found: Vec<(String, &'static str, usize, usize, usize, String, String)>,
}

impl PolicyFinder<'_> {
    fn span(&self, t: &impl ToTokens) -> Option<(usize, usize, usize)> {
        let f = Finder { src: self.src, lines: self.lines.clone(), func: String::new(), found: Vec::new() };
        f.range(t)
    }

    fn push(&mut self, t: &impl ToTokens, kind: &'static str, new: String) {
        let (Some(fun), Some((line, start, end))) = (self.fun.clone(), self.span(t)) else { return };
        let was = self.src[start..end].to_string();
        self.found.push((fun, kind, line, start, end, was, new));
    }
}

/// Mentions the identifier `actor`.
fn mentions_actor(e: &syn::Expr) -> bool {
    e.to_token_stream().to_string().split(|c: char| !c.is_alphanumeric() && c != '_').any(|w| w == "actor")
}

impl<'ast> Visit<'ast> for PolicyFinder<'_> {
    fn visit_item_mod(&mut self, m: &'ast syn::ItemMod) {
        if !is_test(&m.attrs) {
            syn::visit::visit_item_mod(self, m);
        }
    }

    fn visit_item_impl(&mut self, i: &'ast syn::ItemImpl) {
        let is_policy = i
            .trait_
            .as_ref()
            .and_then(|(_, path, _)| path.segments.last())
            .is_some_and(|s| s.ident == "Policy");
        if !is_policy {
            return;
        }
        for item in &i.items {
            let syn::ImplItem::Fn(f) = item else { continue };
            let name = f.sig.ident.to_string();
            self.fun = Some(name.clone());
            if name == "visible" || name == "stored" {
                self.push(&f.block, if name == "visible" { "visible-true" } else { "stored-true" }, "{ true }".into());
            }
            if name == "permits" || name == "visible" {
                syn::visit::visit_block(self, &f.block);
            }
            self.fun = None;
        }
    }

    fn visit_expr_binary(&mut self, e: &'ast syn::ExprBinary) {
        let Some(fun) = self.fun.clone() else { return };
        match e.op {
            syn::BinOp::And(_) if fun == "permits" => {
                let left = self.span(&*e.left).map(|(_, s, t)| self.src[s..t].to_string());
                let right = self.span(&*e.right).map(|(_, s, t)| self.src[s..t].to_string());
                if let (Some(l), Some(r)) = (left, right) {
                    self.push(e, "drop-right-conjunct", l);
                    self.push(e, "drop-left-conjunct", r);
                }
            }
            syn::BinOp::Eq(_) if mentions_actor(&e.left) || mentions_actor(&e.right) => {
                self.push(e, "actor-check-true", "true".into());
            }
            _ => {}
        }
        syn::visit::visit_expr_binary(self, e);
    }
}

/// The declaration a line of Lean starts, if any: its keyword and name.
/// Attributes and modifiers (`@[simp] private theorem t`) are skipped.
fn declaration(line: &str) -> Option<(&str, &str)> {
    let mut w = line.split_whitespace().peekable();
    if w.peek()?.starts_with("@[") {
        // `@[simp, grind]` may span words.
        while !w.next()?.ends_with(']') {}
    }
    let mut first = w.next()?;
    while matches!(first, "private" | "protected" | "noncomputable" | "nonrec" | "partial" | "unsafe") {
        first = w.next()?;
    }
    match first {
        "theorem" | "lemma" | "def" | "instance" | "h5i_authz" => Some((first, w.next()?)),
        _ => None,
    }
}

/// The declarations a failed build reported errors in: each `error:
/// <file>.lean:<line>:` line of `log` as `<file>: <name>`, the `theorem`,
/// `def` or `h5i_authz` command at or above that line of the proofs' file.
pub fn broken_declarations(log: &str, proofs: &Path) -> Vec<String> {
    static ERR: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"error: ([^\s:]+\.lean):(\d+):").unwrap());
    let mut out: Vec<String> = Vec::new();
    for c in ERR.captures_iter(log) {
        let Ok(text) = std::fs::read_to_string(proofs.join(&c[1])) else { continue };
        let line: usize = c[2].parse().unwrap_or(1);
        let found = text.lines().take(line).collect::<Vec<_>>().into_iter().rev().find_map(declaration);
        let file = spec_file(proofs, Path::new(&c[1]));
        let name = format!("{file}: {}", found.map_or_else(|| format!("line {line}"), |(_, n)| n.to_string()));
        if !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

/// A Lean file as specs name it: its path inside `proofs`, `/`-separated,
/// without `./` (Lake prints `././Proofs/T.lean`), so `Spec.lean` and
/// `Model/Spec.lean` are told apart. A path outside `proofs` stays as given.
fn spec_file(proofs: &Path, file: &Path) -> String {
    let rel = file
        .strip_prefix(proofs)
        .ok()
        .or_else(|| file.strip_prefix(proofs.canonicalize().ok()?).ok())
        .unwrap_or(file);
    rel.components()
        .filter(|c| !matches!(c, std::path::Component::CurDir))
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// The specs in `proofs` (`theorem`, `lemma` and `h5i_authz` commands, in
/// every `.lean` file outside `.lake`), named as [`broken_declarations`]
/// names them.
pub fn specs(proofs: &Path) -> Vec<String> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let path = e.path();
            if path.is_dir() {
                if e.file_name() != ".lake" {
                    walk(&path, out);
                }
            } else if path.extension().is_some_and(|x| x == "lean") {
                out.push(path);
            }
        }
    }
    let mut files = Vec::new();
    walk(proofs, &mut files);
    files.sort();
    let mut out = Vec::new();
    for f in files {
        let Ok(text) = std::fs::read_to_string(&f) else { continue };
        let file = spec_file(proofs, &f);
        for (kind, name) in text.lines().filter_map(declaration) {
            let name = format!("{file}: {name}");
            if kind != "def" && kind != "instance" && !out.contains(&name) {
                out.push(name);
            }
        }
    }
    out
}

/// The specs of `p` that no caught mutant broke. Each is a lead, not a
/// verdict: the statement may be vacuous or not constrain what the kernel
/// computes, or simply no mutant aimed at it. A theorem in a module that
/// imports a broken one is not built, so it is never seen breaking either.
pub fn unbroken(p: &Project, outcomes: &[Outcome]) -> Vec<String> {
    if !outcomes.iter().any(|o| o.verdict == Verdict::Caught) {
        return Vec::new();
    }
    let broken: std::collections::HashSet<&str> =
        outcomes.iter().flat_map(|o| o.broke.iter().map(String::as_str)).collect();
    specs(&p.proofs()).into_iter().filter(|s| !broken.contains(s.as_str())).collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The proofs no longer build.
    Caught,
    /// Everything builds: the spec does not see this bug.
    Survived,
    /// Not run: the bug is in code no proof is about, because the server is
    /// not linked to the function a proof covers (`tenant-of-token`).
    ByDesign,
    /// The baseline built.
    Ok,
    /// The edit did not apply, or the mutated Rust or its extraction failed.
    Invalid(&'static str),
}

impl std::fmt::Display for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Verdict::Caught => write!(f, "caught"),
            Verdict::Survived => write!(f, "SURVIVED"),
            Verdict::ByDesign => write!(f, "survives by design (not linked)"),
            Verdict::Ok => write!(f, "ok"),
            Verdict::Invalid(why) => write!(f, "invalid ({why})"),
        }
    }
}

/// A copy of the project to mutate.
struct Sandbox {
    _tmp: tempfile::TempDir,
    project: Project,
    log: PathBuf,
    /// The job slot's lock, held until the mutant's verdict.
    _slot: std::fs::File,
}

/// Lay out a copy of `p` in a temporary directory: the kernel's path crates
/// at their relative places under a fresh workspace, the manifest and the
/// proofs, with the Lake packages and path requires pointing at the original.
fn sandbox(p: &Project, name: &str, slot: usize) -> Result<Sandbox> {
    let krate = p.krate().context("no [extract] crate")?;
    let meta = util::metadata(&krate, true)?;
    let kernel = meta.package_at(&krate)?;
    let locals = meta.local_closure(kernel);
    let dirs: Vec<PathBuf> = locals
        .iter()
        .map(|q| q.manifest_path.parent().unwrap().canonicalize())
        .collect::<std::io::Result<_>>()?;
    let proofs = p.proofs().canonicalize()?;
    let anchor = anchor(&p.root, &dirs, &proofs)?;
    let safe: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .take(40)
        .collect();
    let tmp = tempfile::Builder::new()
        .prefix(&format!("h5i-app-mutant-{safe}-"))
        .tempdir()?;
    let at = |orig: &Path| tmp.path().join(orig.strip_prefix(&anchor).unwrap());

    let mut members = Vec::new();
    let mut ws = WorkspaceTables::default();
    for d in &dirs {
        util::copy_tree(d, &at(d))?;
        strip_dev_dependencies(&at(d).join("Cargo.toml"))?;
        members.push(d.strip_prefix(&anchor)?.to_string_lossy().into_owned());
        ws.absorb(d, &anchor, &dirs)?;
    }
    let lock = meta.workspace_root.join("Cargo.lock");
    if lock.is_file() {
        std::fs::copy(&lock, tmp.path().join("Cargo.lock"))?;
    }
    std::fs::write(tmp.path().join("Cargo.toml"), ws.render(&members)?)?;
    // One target directory per job slot, so two mutants never wait on one
    // another's build lock. Held exclusively: another `h5i app mutate` on the
    // same project waits for the slot rather than building in it at once.
    // The copied crates' fingerprints go, so Cargo rebuilds them even when
    // their timestamps are no newer than the last build in the slot.
    let target = meta.target_directory.join("h5i-app-mutate").join(format!("slot-{slot}"));
    std::fs::create_dir_all(&target)?;
    let lock = std::fs::File::create(target.join("h5i-app-mutate.lock"))?;
    lock.lock().with_context(|| format!("locking {}", target.display()))?;
    let names: Vec<&str> = locals.iter().map(|q| q.name.as_str()).collect();
    forget_fingerprints(&target, &names, 4)?;

    std::fs::create_dir_all(at(&p.root))?;
    std::fs::copy(
        p.root.join(crate::manifest::FILE),
        at(&p.root).join(crate::manifest::FILE),
    )?;
    let copy = at(&proofs);
    if !copy.exists() {
        util::copy_tree(&proofs, &copy)?;
    }
    let packages = proofs.join(".lake/packages");
    if !packages.exists() {
        bail!(
            "{} has no Lake packages yet; run `h5i app check` once first",
            proofs.display()
        );
    }
    std::fs::create_dir_all(copy.join(".lake"))?;
    util::symlink(&packages.canonicalize()?, &copy.join(".lake/packages"))?;
    // The original's build, as a cache: Lake rebuilds what the mutant's
    // re-extraction changed and what imports it. Absent (or stale), Lake
    // builds as it would have.
    let build = proofs.join(".lake/build");
    if build.is_dir() {
        util::copy_tree(&build, &copy.join(".lake/build"))?;
    }
    absolute_path_requires(&proofs, &copy)?;

    let mut project = Project::load(&at(&p.root).join(crate::manifest::FILE))?;
    project.cargo_target = Some(target);
    let log = tmp.path().join("log.txt");
    Ok(Sandbox {
        _tmp: tmp,
        project,
        log,
        _slot: lock,
    })
}

/// Delete the fingerprints of packages `names` in every `.fingerprint`
/// directory of a Cargo target directory (`<profile>/`, `<triple>/<profile>/`,
/// or deeper, as Charon lays its builds out), down to `depth` levels. Cargo
/// then rebuilds those packages and nothing else.
fn forget_fingerprints(dir: &Path, names: &[&str], depth: usize) -> Result<()> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Ok(()) };
    for e in rd.flatten() {
        if !e.file_type()?.is_dir() {
            continue;
        }
        let path = e.path();
        if e.file_name() == ".fingerprint" {
            for f in std::fs::read_dir(&path)?.flatten() {
                let file = f.file_name();
                let file = file.to_string_lossy();
                // `<package>-<hash>`: the package name may itself hold `-`.
                let ours = names.iter().any(|n| {
                    file.strip_prefix(n)
                        .and_then(|r| r.strip_prefix('-'))
                        .is_some_and(|h| !h.is_empty() && h.chars().all(|c| c.is_ascii_hexdigit()))
                });
                if ours {
                    std::fs::remove_dir_all(f.path())?;
                }
            }
        } else if depth > 0 {
            forget_fingerprints(&path, names, depth - 1)?;
        }
    }
    Ok(())
}

/// The directory the copy mirrors: the deepest common ancestor of the
/// project root, the crates and the proofs that is not itself a crate, since
/// the copy's root gets the fresh workspace `Cargo.toml` (a kernel crate may
/// be the project root, as in `h5i-app-h1`).
fn anchor(root: &Path, dirs: &[PathBuf], proofs: &Path) -> Result<PathBuf> {
    let mut anchor = root.to_path_buf();
    for d in dirs.iter().map(PathBuf::as_path).chain([proofs]) {
        while !d.starts_with(&anchor) {
            anchor = anchor.parent().context("no common ancestor")?.to_path_buf();
        }
    }
    while dirs.contains(&anchor) {
        anchor = anchor.parent().context("crate at the filesystem root")?.to_path_buf();
    }
    Ok(anchor)
}

/// Dev-dependencies are never built here, but Cargo would still resolve them,
/// and they may point at crates the copy does not have.
fn strip_dev_dependencies(manifest: &Path) -> Result<()> {
    let text = std::fs::read_to_string(manifest)?;
    let mut doc: toml::Table = toml::from_str(&text)?;
    if doc.remove("dev-dependencies").is_some() {
        std::fs::write(manifest, toml::to_string(&doc)?)?;
    }
    Ok(())
}

/// What the copied crates inherit with `workspace = true`, gathered from the
/// workspaces they came from.
#[derive(Default)]
struct WorkspaceTables {
    package: toml::Table,
    dependencies: toml::Table,
    lints: toml::Table,
    seen: Vec<PathBuf>,
}

impl WorkspaceTables {
    fn absorb(&mut self, crate_dir: &Path, anchor: &Path, copied: &[PathBuf]) -> Result<()> {
        let Some(root) = crate_dir
            .ancestors()
            .map(|a| a.join("Cargo.toml"))
            .find(|m| {
                std::fs::read_to_string(m)
                    .ok()
                    .and_then(|t| t.parse::<toml::Table>().ok())
                    .is_some_and(|t| t.contains_key("workspace"))
            })
        else {
            return Ok(());
        };
        if self.seen.contains(&root) {
            return Ok(());
        }
        self.seen.push(root.clone());
        let doc: toml::Table = std::fs::read_to_string(&root)?.parse()?;
        let ws_dir = root.parent().unwrap();
        let Some(ws) = doc.get("workspace").and_then(|w| w.as_table()) else {
            return Ok(());
        };
        let merge = |into: &mut toml::Table, key: &str| {
            if let Some(t) = ws.get(key).and_then(|v| v.as_table()) {
                for (k, v) in t {
                    into.entry(k.clone()).or_insert(v.clone());
                }
            }
        };
        merge(&mut self.package, "package");
        merge(&mut self.lints, "lints");
        if let Some(deps) = ws.get("dependencies").and_then(|v| v.as_table()) {
            for (k, v) in deps {
                let mut v = v.clone();
                if let Some(path) = v.get("path").and_then(|p| p.as_str()) {
                    let target = ws_dir
                        .join(path)
                        .canonicalize()
                        .unwrap_or_else(|_| ws_dir.join(path));
                    if !copied.contains(&target) {
                        continue;
                    }
                    let rel = target.strip_prefix(anchor)?.to_string_lossy().into_owned();
                    v.as_table_mut()
                        .unwrap()
                        .insert("path".into(), toml::Value::String(rel));
                }
                self.dependencies.entry(k.clone()).or_insert(v);
            }
        }
        Ok(())
    }

    fn render(&self, members: &[String]) -> Result<String> {
        let mut ws = toml::Table::new();
        ws.insert("resolver".into(), "3".into());
        ws.insert(
            "members".into(),
            toml::Value::Array(members.iter().map(|m| m.clone().into()).collect()),
        );
        for (k, t) in [
            ("package", &self.package),
            ("dependencies", &self.dependencies),
            ("lints", &self.lints),
        ] {
            if !t.is_empty() {
                ws.insert(k.into(), toml::Value::Table(t.clone()));
            }
        }
        let mut doc = toml::Table::new();
        doc.insert("workspace".into(), toml::Value::Table(ws));
        Ok(toml::to_string(&doc)?)
    }
}

/// A Lake `require … from "../relative"` would point inside the copy, where
/// that package is not: point it at the original, already built.
fn absolute_path_requires(orig: &Path, copy: &Path) -> Result<()> {
    let manifest = orig.join("lake-manifest.json");
    let Ok(text) = std::fs::read_to_string(&manifest) else {
        return Ok(());
    };
    let json: serde_json::Value = serde_json::from_str(&text)?;
    let mut dirs = Vec::new();
    for pkg in json
        .get("packages")
        .and_then(|p| p.as_array())
        .into_iter()
        .flatten()
    {
        if pkg.get("type").and_then(|t| t.as_str()) == Some("path")
            && let Some(dir) = pkg.get("dir").and_then(|d| d.as_str())
            && Path::new(dir).is_relative()
        {
            let abs = orig
                .join(dir)
                .canonicalize()
                .with_context(|| format!("Lake path package {dir}"))?;
            dirs.push((dir.to_string(), abs.to_string_lossy().into_owned()));
        }
    }
    for f in ["lakefile.lean", "lakefile.toml", "lake-manifest.json"] {
        let path = copy.join(f);
        let Ok(mut text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for (rel, abs) in &dirs {
            text = text.replace(&format!("\"{rel}\""), &format!("\"{abs}\""));
        }
        std::fs::write(&path, text)?;
    }
    Ok(())
}

fn evaluate(
    p: &Project,
    m: Option<&Mutant>,
    slot: usize,
    targets: &[String],
    keep: bool,
    observe: &mut impl FnMut(h5i_app_report::Stage) -> Result<()>,
) -> Result<(Verdict, Option<PathBuf>, Vec<String>)> {
    let name = m.map_or("baseline", |m| m.name.as_str());
    // A mutant of code no proof is about (a server's own principal
    // function): it survives by design, and is not run.
    if let Some(Mutant { change: Change::NotRun { .. }, .. }) = m {
        return Ok((Verdict::ByDesign, None, Vec::new()));
    }
    let sb = measured("sandbox", None, observe, || sandbox(p, name, slot))?;
    let out = Out::Log(sb.log.clone());
    let verdict = (|| -> Result<Verdict> {
        if let Some(m) = m {
            let edit = measured("edit", Some(&sb.log), observe, || {
                let file = sb.project.root.join(&m.file);
                let src = std::fs::read_to_string(&file)
                    .with_context(|| format!("reading {}", file.display()))?;
                std::fs::write(&file, m.apply(&src)?)?;
                Ok(())
            });
            if edit.is_err() {
                return Ok(Verdict::Invalid("edit"));
            }
        }
        let krate = sb.project.krate().unwrap();
        if !measured_status("rust_check", &sb.log, observe, || {
            let mut cargo = Command::new("cargo");
            if let Some(t) = &sb.project.cargo_target {
                cargo.env("CARGO_TARGET_DIR", t);
            }
            util::status(cargo.args(["check", "--release", "-q"]).current_dir(&krate), &out)
        })? {
            return Ok(Verdict::Invalid("rust"));
        }
        if let Err(e) = measured("extraction", Some(&sb.log), observe, || {
            crate::extract::extract_kernel(&sb.project, &out)
        }) {
            out.note(&format!("{e:#}"));
            return Ok(Verdict::Invalid("extraction"));
        }
        // A failed build is reported as a proof-stage failure, not a named
        // theorem rejecting the mutant. The diagnostics remain available.
        let built = measured_status("lean_build", &sb.log, observe, || {
            util::status(
                Command::new("lake")
                    .arg("build")
                    .args(targets)
                    .current_dir(sb.project.proofs()),
                &out,
            )
        })?;
        Ok(match (m.is_some(), built) {
            (false, true) => Verdict::Ok,
            (false, false) => Verdict::Invalid("baseline does not build"),
            (true, true) => Verdict::Survived,
            (true, false) => Verdict::Caught,
        })
    })()?;
    // `tenant-of-token` proven by `check`'s automation, not by a theorem the
    // build compiles: run that automation too, so a mutant that breaks it is
    // caught. The baseline must pass it.
    let mut broke = Vec::new();
    let verdict = match verdict {
        Verdict::Ok | Verdict::Survived => match tenant_automation(&sb.project, observe, &sb.log, &out)? {
            Some(false) if m.is_none() => Verdict::Invalid("baseline: tenant-of-token automation fails"),
            Some(false) => {
                broke.push(format!("{} (automation)", crate::obligations::TENANT_OF_TOKEN));
                Verdict::Caught
            }
            _ => verdict,
        },
        v => v,
    };
    // Which specs the mutant broke, read off the failed build's errors.
    if verdict == Verdict::Caught && broke.is_empty() {
        broke = broken_declarations(&std::fs::read_to_string(&sb.log).unwrap_or_default(), &sb.project.proofs());
    }
    let kept = if keep { Some(sb._tmp.keep()) } else { None };
    Ok((verdict, kept, broke))
}

/// `check`'s automation for `tenant-of-token`, when the project opts in
/// without naming a theorem: `Some(proven)`, or `None` when it does not apply.
fn tenant_automation(
    p: &Project,
    observe: &mut impl FnMut(h5i_app_report::Stage) -> Result<()>,
    log: &Path,
    out: &Out,
) -> Result<Option<bool>> {
    let obligations = crate::obligations::of(p)?;
    let Some(o) = obligations.iter().find(|o| o.name == crate::obligations::TENANT_OF_TOKEN && o.theorem.is_none() && o.auto.is_some())
    else {
        return Ok(None);
    };
    let proofs = p.proofs();
    measured("tenant_automation", Some(log), observe, || {
        util::run(
            Command::new("lake").args(["build", crate::check::OBLIGATION_MODULE]).current_dir(&proofs),
            out,
        )?;
        let modules = crate::check::generated_modules(&proofs, p);
        let proven = crate::check::automation(&proofs, &modules, &[o], &crate::check::nonce(), out)?;
        Ok(Some(proven.get(o.name).copied().unwrap_or(false)))
    })
}

fn measured<T>(
    name: &str,
    log: Option<&Path>,
    observe: &mut impl FnMut(h5i_app_report::Stage) -> Result<()>,
    f: impl FnOnce() -> Result<T>,
) -> Result<T> {
    use h5i_app_report::Stage;
    observe(Stage {
        name: name.into(),
        status: "running".into(),
        seconds: 0.0,
        log: String::new(),
    })?;
    let time = Instant::now();
    let result = f();
    let mut text = log.map(crate::report::tail).unwrap_or_default();
    if let Err(e) = &result {
        text.push_str(&format!("\n{e:#}"));
    }
    observe(Stage {
        name: name.into(),
        status: if result.is_ok() { "passed" } else { "failed" }.into(),
        seconds: time.elapsed().as_secs_f64(),
        log: text,
    })?;
    result
}

/// A process that could not start is not a nonzero exit and must not count as
/// a mutant caught by Lean. Preserve both distinctions in the persisted stage.
fn measured_status(
    name: &str,
    log: &Path,
    observe: &mut impl FnMut(h5i_app_report::Stage) -> Result<()>,
    f: impl FnOnce() -> Result<bool>,
) -> Result<bool> {
    let mut final_stage = None;
    let result = measured(
        name,
        Some(log),
        &mut |stage| {
            if stage.status == "running" {
                observe(stage)
            } else {
                final_stage = Some(stage);
                Ok(())
            }
        },
        f,
    );
    if let Some(mut stage) = final_stage {
        stage.status = match &result {
            Ok(true) => "passed",
            Ok(false) => "failed",
            Err(_) => "error",
        }
        .into();
        observe(stage)?;
    }
    result
}

fn record_stage(
    record: &Arc<Mutex<h5i_app_report::Recorder>>,
    index: usize,
    stage: h5i_app_report::Stage,
) -> Result<()> {
    let mut r = record.lock().unwrap();
    let trial = &mut r.run.trials[index];
    trial.status = "running".into();
    if trial.stages.last().is_some_and(|s| s.name == stage.name) {
        trial.stages.pop();
    }
    trial.stages.push(stage);
    Ok(r.save()?)
}

fn record_verdict(
    record: &Arc<Mutex<h5i_app_report::Recorder>>,
    index: usize,
    verdict: Verdict,
) -> Result<()> {
    let mut r = record.lock().unwrap();
    r.run.trials[index].status = match verdict {
        Verdict::Caught => "proof_failed",
        Verdict::Survived => "survived",
        Verdict::ByDesign => "survived_by_design",
        Verdict::Ok => "passed",
        Verdict::Invalid("error") => "error",
        Verdict::Invalid(_) => "invalid",
    }
    .into();
    if index == 0 {
        r.run.baseline = r.run.trials[index].status.clone();
    }
    Ok(r.save()?)
}

#[derive(Clone, Copy)]
pub struct Options {
    pub jobs: usize,
    pub keep: bool,
    pub baseline: bool,
}

/// One mutant's outcome.
#[derive(Debug)]
pub struct Outcome {
    pub name: String,
    pub verdict: Verdict,
    /// Generated rather than declared in the manifest.
    pub generated: bool,
    /// For a caught mutant, the declarations whose proofs failed
    /// (`file: name`), read off the build's errors.
    pub broke: Vec<String>,
}

/// Run `mutants` against `p`, printing one line each.
pub fn run(p: &Project, mutants: Vec<Mutant>, opts: &Options) -> Result<Vec<Outcome>> {
    let targets = &p.manifest.mutate.targets;
    use h5i_app_report::{Edit as RecordedEdit, Trial};
    // Mutants that are not run (surviving by design) need no baseline.
    let baseline = opts.baseline && mutants.iter().any(|m| !matches!(m.change, Change::NotRun { .. }));
    let opts = &Options { baseline, ..*opts };
    let mut record = crate::report::start(p, "mutation", targets.clone())?;
    record.run.baseline = if opts.baseline { "pending" } else { "skipped" }.into();
    record.run.trials.push(Trial {
        name: "baseline".into(),
        generated: false,
        file: String::new(),
        edits: vec![],
        status: record.run.baseline.clone(),
        stages: vec![],
    });
    for m in &mutants {
        let edits = match &m.change {
            Change::Replace { edits: es, .. } => es
                .iter()
                .map(|e| RecordedEdit {
                    before: e.old.clone(),
                    after: e.new.clone(),
                })
                .collect(),
            Change::Splice { was, new, .. } => vec![RecordedEdit {
                before: was.clone(),
                after: new.clone(),
            }],
            Change::NotRun { .. } => vec![],
        };
        record.run.trials.push(Trial {
            name: m.name.clone(),
            generated: m.what.is_some(),
            file: m.file.to_string_lossy().into_owned(),
            edits,
            status: "pending".into(),
            stages: vec![],
        });
    }
    record.save()?;
    let record = Arc::new(Mutex::new(record));
    if opts.baseline {
        eprint!("{:<48} ", "baseline (the unmodified kernel, copied)");
        let t = Instant::now();
        let baseline = evaluate(p, None, 0, targets, opts.keep, &mut |stage| {
            record_stage(&record, 0, stage)
        });
        let (v, kept, _) = match baseline {
            Ok(value) => value,
            Err(e) => {
                let mut r = record.lock().unwrap();
                r.run.baseline = "error".into();
                r.run.trials[0].status = "error".into();
                for t in &mut r.run.trials[1..] {
                    t.status = "skipped".into();
                }
                r.run.error = Some(format!("{e:#}"));
                r.finish("error")?;
                return Err(e);
            }
        };
        record_verdict(&record, 0, v)?;
        eprintln!("{v}  {:.0}s{}", t.elapsed().as_secs_f32(), kept_note(&kept));
        if v != Verdict::Ok {
            let mut r = record.lock().unwrap();
            for t in &mut r.run.trials[1..] {
                t.status = "skipped".into();
            }
            r.run.error = Some("Baseline failed; mutations were not executed".into());
            r.finish("failed")?;
            bail!(
                "the unmodified project does not build in a copy, so no mutant would mean anything{}",
                kept_note(&kept)
            );
        }
    }
    let total = mutants.len();
    let queue = Arc::new(Mutex::new(
        mutants.into_iter().enumerate().collect::<VecDeque<_>>(),
    ));
    let results = Arc::new(Mutex::new(Vec::new()));
    let recording_errors = Arc::new(Mutex::new(Vec::new()));
    std::thread::scope(|s| {
        for slot in 0..opts.jobs.max(1) {
            let (queue, results) = (queue.clone(), results.clone());
            let (record, recording_errors) = (record.clone(), recording_errors.clone());
            s.spawn(move || {
                loop {
                    let Some((i, m)) = queue.lock().unwrap().pop_front() else {
                        break;
                    };
                    let t = Instant::now();
                    let (v, kept, broke) = match evaluate(p, Some(&m), slot, targets, opts.keep, &mut |stage| {
                        let result = record_stage(&record, i + 1, stage);
                        if let Err(e) = &result {
                            recording_errors.lock().unwrap().push(e.to_string());
                        }
                        result
                    }) {
                        Ok(r) => r,
                        Err(e) => {
                            eprintln!("{:<48} error: {e:#}", m.name);
                            (Verdict::Invalid("error"), None, Vec::new())
                        }
                    };
                    if let Err(e) = record_verdict(&record, i + 1, v) {
                        recording_errors.lock().unwrap().push(e.to_string());
                    }
                    let by = if broke.is_empty() { String::new() } else { format!(" by {}", broke.join(", ")) };
                    let by = match &m.change {
                        Change::NotRun { why } => format!(": {why}"),
                        _ => by,
                    };
                    eprintln!(
                        "[{}/{total}] {:<40} {v}{by}  {:.0}s{}",
                        i + 1,
                        m.name,
                        t.elapsed().as_secs_f32(),
                        kept_note(&kept)
                    );
                    let o = Outcome {
                        name: m.name.clone(),
                        verdict: v,
                        generated: m.what.is_some(),
                        broke,
                    };
                    results.lock().unwrap().push((i, o));
                }
            });
        }
    });
    let mut r = Arc::try_unwrap(results).unwrap().into_inner().unwrap();
    r.sort_by_key(|(i, _)| *i);
    let mut record = record.lock().unwrap();
    let errors = recording_errors.lock().unwrap();
    if !errors.is_empty() {
        record.run.error = Some(errors.join("\n"));
        record.finish("error")?;
        bail!("could not persist mutation evidence: {}", errors.join("; "));
    }
    record.finish(if r.iter().any(|(_, o)| o.verdict != Verdict::Caught) {
        "completed_with_issues"
    } else {
        "completed"
    })?;
    Ok(r.into_iter().map(|(_, o)| o).collect())
}

fn kept_note(kept: &Option<PathBuf>) -> String {
    kept.as_ref()
        .map(|k| format!("  ({})", k.display()))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_errors_are_not_proof_failures_and_logs_survive() {
        let log = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(log.path(), "diagnostic from Lean").unwrap();
        let mut stages = vec![];
        let result = measured_status(
            "lean_build",
            log.path(),
            &mut |s| {
                stages.push(s);
                Ok(())
            },
            || bail!("lake not installed"),
        );
        assert!(result.is_err());
        assert_eq!(stages[1].status, "error");
        assert!(stages[1].log.contains("lake not installed"));
        stages.clear();
        let built = measured_status(
            "lean_build",
            log.path(),
            &mut |s| {
                stages.push(s);
                Ok(())
            },
            || Ok(false),
        )
        .unwrap();
        assert!(!built);
        assert_eq!(stages[1].status, "failed");
        drop(log);
        assert!(stages[1].log.contains("diagnostic from Lean"));
    }

    fn found(src: &str) -> Vec<(String, String, String)> {
        let ast = syn::parse_file(src).unwrap();
        let mut v = Finder {
            src,
            lines: line_starts(src),
            func: String::new(),
            found: Vec::new(),
        };
        v.visit_file(&ast);
        v.found
            .into_iter()
            .map(|(f, _, _, _, was, new)| (f, was, new))
            .collect()
    }

    #[test]
    fn finds_operators_and_conditions() {
        let src = "fn f(a: u64, b: u64) -> bool {\n    if a <= b && !(a == 0) { true } else { false }\n}\n#[cfg(test)]\nmod t { fn g() -> bool { 1 < 2 } }\n";
        let got = found(src);
        let want = [
            ("f", "a <= b && !(a == 0)", "true"),
            ("f", "a <= b && !(a == 0)", "false"),
            ("f", "&&", "||"),
            ("f", "<=", "<"),
            ("f", "!", ""),
            ("f", "==", "!="),
        ];
        let got: Vec<(&str, &str, &str)> = got
            .iter()
            .map(|(a, b, c)| (a.as_str(), b.as_str(), c.as_str()))
            .collect();
        assert_eq!(got, want);
    }

    #[test]
    fn splices_apply_by_offset() {
        let src = "fn f(a: u64) -> bool { a >= 1 }";
        let ast = syn::parse_file(src).unwrap();
        let mut v = Finder {
            src,
            lines: line_starts(src),
            func: String::new(),
            found: Vec::new(),
        };
        v.visit_file(&ast);
        let (_, _, start, end, was, new) = v.found[0].clone();
        let m = Mutant {
            name: "x".into(),
            what: None,
            file: "a.rs".into(),
            change: Change::Splice {
                start,
                end,
                was,
                new,
            },
        };
        assert_eq!(m.apply(src).unwrap(), "fn f(a: u64) -> bool { a > 1 }");
    }

    #[test]
    fn the_principal_mutant_runs_only_when_linked() {
        let d = tempfile::tempdir().unwrap();
        let w = |f: &str, t: &str| {
            let p = d.path().join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, t).unwrap();
        };
        w("h5i-app.toml", "[extract]\n[authz]\nguarded = [\"guarded_transition\"]\n[obligations]\nprincipal = \"principal\"\n");
        w("kernel/Cargo.toml", "[package]\nname = \"k\"\n");
        let kernel = "pub fn principal(org: u64, user: u64) -> P {\n    P { org, user }\n}\n";
        w("kernel/src/lib.rs", kernel);
        w("server/Cargo.toml", "[package]\nname = \"s\"\n[dependencies]\naxum = \"0.8\"\n");
        w("server/src/main.rs", "fn main() { HmacAuth::new(s, k::principal); }\n");
        let p = Project::load(&d.path().join("h5i-app.toml")).unwrap();
        let files = [d.path().join("kernel/src/lib.rs")];
        let m = principal_mutant(&p, &files).unwrap().unwrap();
        assert_eq!(m.file, PathBuf::from("kernel/src/lib.rs"));
        assert_eq!(
            m.apply(kernel).unwrap(),
            "pub fn principal(org: u64, user: u64) -> P {\n    let org: u64 = 0;\n    P { org, user }\n}\n"
        );
        // The server's own closure: no proof is about it, so it is not run.
        w("server/src/main.rs", "fn main() { HmacAuth::new(s, |t, u| P { org: t, user: u }); }\n");
        let m = principal_mutant(&p, &files).unwrap().unwrap();
        match &m.change {
            Change::NotRun { why } => assert!(why.contains("server/src/main.rs:1") && why.contains("closure"), "{why}"),
            c => panic!("{c:?}"),
        }
        // Nothing opted in and no authenticator: no mutant.
        w("h5i-app.toml", "[extract]\n");
        std::fs::remove_dir_all(d.path().join("server")).unwrap();
        let p = Project::load(&d.path().join("h5i-app.toml")).unwrap();
        assert!(principal_mutant(&p, &files).unwrap().is_none());
    }

    #[test]
    fn the_sandbox_keeps_a_root_crate_manifest() {
        let d = tempfile::tempdir().unwrap();
        let w = |f: &str, t: &str| {
            let p = d.path().join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, t).unwrap();
        };
        // The kernel crate is the project root, inside a workspace.
        w("Cargo.toml", "[workspace]\nmembers = [\"k\"]\n[workspace.package]\nedition = \"2024\"\n");
        w("k/Cargo.toml", "[package]\nname = \"k\"\nversion = \"0.1.0\"\nedition.workspace = true\n");
        w("k/src/lib.rs", "pub fn f() {}\n");
        w("k/h5i-app.toml", "[extract]\ncrate = \".\"\n");
        std::fs::create_dir_all(d.path().join("k/proofs/.lake/packages")).unwrap();
        w("k/proofs/.lake/build/lib/lean/K.olean", "olean");
        let p = Project::load(&d.path().join("k/h5i-app.toml")).unwrap();
        let s = sandbox(&p, "root-crate", 3).unwrap();
        let copy = &s.project.root;
        let own = std::fs::read_to_string(copy.join("Cargo.toml")).unwrap();
        assert!(own.contains("name = \"k\""), "{own}");
        let ws = std::fs::read_to_string(copy.parent().unwrap().join("Cargo.toml")).unwrap();
        assert!(ws.contains("members = [\"k\"]") && ws.contains("edition = \"2024\""), "{ws}");
        assert!(copy.join("proofs/.lake/packages").exists());
        // The Lake build is copied as a cache; Cargo builds in the slot's
        // target directory, under the original's.
        assert_eq!(std::fs::read_to_string(copy.join("proofs/.lake/build/lib/lean/K.olean")).unwrap(), "olean");
        util::metadata(copy, false).unwrap().package_at(copy).unwrap();
        let orig = util::metadata(&d.path().join("k"), false).unwrap().target_directory;
        let slot = orig.join("h5i-app-mutate/slot-3");
        assert_eq!(s.project.cargo_target.as_deref(), Some(slot.as_path()));
        // The slot is held: a second copy in it waits (here: cannot lock).
        let other = std::fs::File::open(slot.join("h5i-app-mutate.lock")).unwrap();
        assert!(other.try_lock().is_err());
        drop(s);
        assert!(other.try_lock().is_ok());
    }

    #[test]
    fn only_the_copied_crates_fingerprints_are_forgotten() {
        let d = tempfile::tempdir().unwrap();
        for f in [
            "release/.fingerprint/k-0123abcd",
            "release/.fingerprint/k-extra-0123abcd",
            "release/.fingerprint/serde-0123abcd",
            "x86_64-unknown-linux-gnu/debug/.fingerprint/k-ffff",
        ] {
            std::fs::create_dir_all(d.path().join(f)).unwrap();
        }
        forget_fingerprints(d.path(), &["k"], 4).unwrap();
        let left = |f: &str| d.path().join(f).exists();
        assert!(!left("release/.fingerprint/k-0123abcd"));
        assert!(!left("x86_64-unknown-linux-gnu/debug/.fingerprint/k-ffff"));
        assert!(left("release/.fingerprint/k-extra-0123abcd"));
        assert!(left("release/.fingerprint/serde-0123abcd"));
    }

    #[test]
    fn specs_are_named_as_broken_declarations_name_them() {
        let d = tempfile::tempdir().unwrap();
        let w = |f: &str, t: &str| {
            let p = d.path().join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, t).unwrap();
        };
        w(
            "Theorems.lean",
            "def helper := 1\n@[simp, grind] private theorem a : True := by\n  trivial\nlemma b : True := trivial\n",
        );
        w("generated/Authz.lean", "h5i_authz guarded\n");
        // Same file name, same theorem name: two specs, not one.
        w("Model/Theorems.lean", "theorem a : True := trivial\n");
        w(".lake/build/X.lean", "theorem hidden : True := trivial\n");
        let specs = specs(d.path());
        assert_eq!(
            specs,
            ["Model/Theorems.lean: a", "Theorems.lean: a", "Theorems.lean: b", "generated/Authz.lean: guarded"]
        );
        // Lake's `././` prefix, and an absolute path, name the same spec.
        let log = format!(
            "error: ././Theorems.lean:3:2: unsolved goals\nerror: {}:1:0: x\n",
            d.path().join("Model/Theorems.lean").display()
        );
        assert_eq!(broken_declarations(&log, d.path()), ["Theorems.lean: a", "Model/Theorems.lean: a"]);
    }

    #[test]
    fn replacements_must_match_once() {
        let m = Mutant {
            name: "x".into(),
            what: None,
            file: "a.rs".into(),
            change: Change::Replace {
                edits: vec![Edit {
                    old: "a".into(),
                    new: "b".into(),
                }],
                function: None,
            },
        };
        assert!(m.apply("aa").is_err());
        assert_eq!(m.apply("xa").unwrap(), "xb");
    }

    #[test]
    fn replacements_can_be_scoped_to_a_function() {
        let src = "fn f(x: u64) -> bool { x > 0 }\n\
                   struct A;\nimpl A { fn g(&self, x: u64) -> bool { x > 0 } }\n\
                   struct B;\nimpl Tr for B { fn g(&self, x: u64) -> bool { x > 0 } }\n";
        let m = |function: Option<&str>| Mutant {
            name: "x".into(),
            what: None,
            file: "a.rs".into(),
            change: Change::Replace {
                edits: vec![Edit { old: "x > 0".into(), new: "x >= 0".into() }],
                function: function.map(Into::into),
            },
        };
        // Three in the file; one in each function.
        assert!(m(None).apply(src).is_err());
        let f = m(Some("f")).apply(src).unwrap();
        assert!(f.starts_with("fn f(x: u64) -> bool { x >= 0 }"));
        assert_eq!(f.matches("x > 0").count(), 2);
        // `g` is ambiguous; `B::g` is the trait impl's.
        assert!(m(Some("g")).apply(src).unwrap_err().to_string().contains("2 functions named `g`"));
        let b = m(Some("B::g")).apply(src).unwrap();
        assert!(b.contains("impl A { fn g(&self, x: u64) -> bool { x > 0 } }"));
        assert!(b.contains("impl Tr for B { fn g(&self, x: u64) -> bool { x >= 0 } }"));
        assert!(m(Some("h")).apply(src).is_err());
    }
}
