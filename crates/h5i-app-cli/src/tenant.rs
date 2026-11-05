//! The tenant of a request: `tenant-of-token`, and whether the server is
//! linked to the principal function it is proven for.
//!
//! The obligation (`crate::obligations`, `H5iAppLib.Obligation.TenantOfToken`)
//! says the kernel's `tenant_of` of what its principal function returns is
//! the token's tenant. It covers the server only if the server builds its
//! principals with that function. This module reads the server crates (syn,
//! [`crate::rust::Sources`]) for that:
//!
//! - every `HmacAuth::new(secret, f)` in a server crate, outside tests, is
//!   *linked* when `f` is the kernel's principal function by path
//!   (`kernel_crate::principal`, through a `use kernel_crate as k` alias or a
//!   `use kernel_crate::principal` import in the same file), or a closure
//!   `|t, u| <that path>(t, u)` / `|t, u| <that path>(t, u, x)` passing its
//!   two arguments in order;
//! - a hand-written `impl Authenticator` is not linked: the server
//!   authenticates its own way;
//! - `or_anonymous` and `Actor::assume_authenticated` decide a principal
//!   without a token, and are listed for review whatever the status.
//!
//! `linked` is a source-level check, not a proof: it reads what the code
//! says, and code only a macro produces is not seen. Nothing here fails a
//! command; it is reported (`h5i app report`, `check`'s run record).

use std::path::Path;

use h5i_app_report::TenantLink;

use crate::manifest::Project;
use crate::rust::Sources;

/// One authenticator construction in a server crate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Construction {
    /// `file:line`, relative to the project.
    pub at: String,
    /// What it is given, as written.
    pub what: String,
    /// The kernel's principal function, by path.
    pub linked: bool,
    /// Why not, when not linked.
    pub why: Option<String>,
}

/// What the server crates say about how a request becomes a principal.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Linkage {
    pub constructions: Vec<Construction>,
    /// `file:line` of each hand-written `impl Authenticator`.
    pub custom: Vec<String>,
    /// `file:line: name` of each `or_anonymous` / `assume_authenticated`.
    pub reviewed: Vec<String>,
    /// Whether the project has a server crate at all.
    pub servers: usize,
    /// Files that do not parse.
    pub errors: Vec<String>,
}

impl Linkage {
    /// At least one construction, all of them linked, no hand-written
    /// authenticator, and every server source parsed.
    pub fn linked(&self) -> bool {
        !self.constructions.is_empty()
            && self.constructions.iter().all(|c| c.linked)
            && self.custom.is_empty()
            && self.errors.is_empty()
    }

    /// Why the server is not linked, in one sentence; `None` when it is.
    pub fn why_not(&self) -> Option<String> {
        if self.linked() {
            return None;
        }
        let mut why = Vec::new();
        if self.servers == 0 {
            why.push("the project has no server crate, so nothing builds principals with it".to_string());
        } else if self.constructions.is_empty() && self.custom.is_empty() {
            why.push("no `HmacAuth::new` in the server crates (lint reads no other authenticator)".to_string());
        }
        for c in self.constructions.iter().filter(|c| !c.linked) {
            why.push(format!("{}: {}", c.at, c.why.clone().unwrap_or_default()));
        }
        for c in &self.custom {
            why.push(format!("{c}: a hand-written `impl Authenticator`: the server authenticates its own way"));
        }
        for e in &self.errors {
            why.push(format!("{e} (not read)"));
        }
        Some(why.join("; "))
    }
}

/// Read the project's server crates (the ones `lint` treats as servers).
pub fn linkage(p: &Project) -> Linkage {
    let mut l = Linkage::default();
    let kernel_dir = p.krate().and_then(|k| k.canonicalize().ok());
    let kernel = p.krate().and_then(|k| crate::util::package_name(&k)).map(|n| crate::util::ident(&n));
    let mut servers = Vec::new();
    crate::rust::crates_under(&p.root, &mut servers);
    servers.retain(|c| crate::lint::is_server(c) && c.canonicalize().ok() != kernel_dir);
    l.servers = servers.len();
    let principal: Vec<String> = p.principal_fn().unwrap_or("principal").split("::").map(str::to_string).collect();
    for s in &servers {
        let src = Sources::of_crate(s);
        l.errors.extend(src.errors.iter().map(|e| rel(p, e)));
        for (file, _, items) in &src.files {
            let uses = Uses::of(items);
            let mut v = Finder { hits: Vec::new() };
            for i in items {
                syn::visit::Visit::visit_item(&mut v, i);
            }
            for (line, arg) in v.hits {
                let what = quote::quote!(#arg).to_string().replace(" :: ", "::");
                let (linked, why) = match kernel.as_deref().filter(|_| p.principal_fn().is_some()) {
                    None => (false, Some(format!("passes `{what}`; no `[obligations] principal` names a kernel function"))),
                    Some(k) => judge(&arg, k, &principal, &uses),
                };
                l.constructions.push(Construction { at: format!("{}:{line}", rel_path(p, file)), what, linked, why });
            }
        }
        for i in src.impls_of("Authenticator") {
            l.custom.push(format!("{}:{}", rel_path(p, &i.file), i.line));
        }
        for (f, line, n) in src.calls_to(&["or_anonymous", "assume_authenticated"]) {
            l.reviewed.push(format!("{}:{line}: `{n}`", rel_path(p, &f)));
        }
    }
    l
}

/// Whether `arg`, given to `HmacAuth::new`, is the kernel's principal
/// function `principal` (path segments) of crate `kernel`.
fn judge(arg: &syn::Expr, kernel: &str, principal: &[String], uses: &Uses) -> (bool, Option<String>) {
    let shown = principal.join("::");
    let rules = format!(
        "lint resolves `{kernel}::{shown}`, a `use {kernel} as k` alias or a `use {kernel}::{shown}` import in the same file"
    );
    match arg {
        syn::Expr::Path(x) if x.qself.is_none() => {
            if resolves(&x.path, kernel, principal, uses) {
                (true, None)
            } else {
                let what = quote::quote!(#x).to_string().replace(' ', "");
                (false, Some(format!("passes `{what}`, not the kernel's `{shown}` ({rules})")))
            }
        }
        syn::Expr::Closure(c) => {
            let params: Vec<Option<String>> = c.inputs.iter().map(pat_ident).collect();
            let body = match &*c.body {
                syn::Expr::Block(b) if b.block.stmts.len() == 1 => match &b.block.stmts[0] {
                    syn::Stmt::Expr(e, None) => e,
                    _ => &*c.body,
                },
                e => e,
            };
            let ok = match (params.as_slice(), body) {
                ([Some(t), Some(u)], syn::Expr::Call(call)) => {
                    let callee = matches!(&*call.func, syn::Expr::Path(x) if x.qself.is_none() && resolves(&x.path, kernel, principal, uses));
                    let args: Vec<&syn::Expr> = call.args.iter().collect();
                    callee && (args.len() == 2 || args.len() == 3) && is_ident(args[0], t) && is_ident(args[1], u)
                }
                _ => false,
            };
            if ok {
                (true, None)
            } else {
                (
                    false,
                    Some(format!(
                        "passes a closure that does not just call the kernel's `{shown}` with its two arguments in order \
                         (`|t, u| {kernel}::{shown}(t, u)`; {rules})"
                    )),
                )
            }
        }
        other => (false, Some(format!("passes `{}`, which lint does not read as a function path", quote::quote!(#other)))),
    }
}

fn pat_ident(p: &syn::Pat) -> Option<String> {
    match p {
        syn::Pat::Ident(i) if i.subpat.is_none() && i.by_ref.is_none() => Some(i.ident.to_string()),
        syn::Pat::Type(t) => pat_ident(&t.pat),
        _ => None,
    }
}

fn is_ident(e: &syn::Expr, name: &str) -> bool {
    matches!(e, syn::Expr::Path(x) if x.qself.is_none() && x.path.leading_colon.is_none() && x.path.is_ident(name))
}

/// A path with no generic arguments, as its segments.
fn plain(path: &syn::Path) -> Option<Vec<String>> {
    path.segments
        .iter()
        .map(|s| s.arguments.is_none().then(|| s.ident.to_string()))
        .collect()
}

fn resolves(path: &syn::Path, kernel: &str, principal: &[String], uses: &Uses) -> bool {
    let Some(segs) = plain(path) else { return false };
    let full: Vec<String> = std::iter::once(kernel.to_string()).chain(principal.iter().cloned()).collect();
    if segs == full {
        return true;
    }
    if path.leading_colon.is_some() {
        return false;
    }
    let Some((first, rest)) = segs.split_first() else { return false };
    // `first` is a name a `use` of this file binds to a path: `use kernel as
    // k` (`k::principal`) or `use kernel::principal` (`principal`).
    match uses.0.iter().find(|(name, _)| name == first) {
        Some((_, target)) => {
            let mut whole = target.clone();
            whole.extend(rest.iter().cloned());
            whole == full
        }
        None => false,
    }
}

/// The names a file's `use` items bind, each to the path it stands for.
struct Uses(Vec<(String, Vec<String>)>);

impl Uses {
    fn of(items: &[syn::Item]) -> Uses {
        fn walk(t: &syn::UseTree, prefix: &mut Vec<String>, out: &mut Vec<(String, Vec<String>)>) {
            match t {
                syn::UseTree::Path(p) => {
                    prefix.push(p.ident.to_string());
                    walk(&p.tree, prefix, out);
                    prefix.pop();
                }
                syn::UseTree::Name(n) => {
                    let name = n.ident.to_string();
                    if name == "self" {
                        if let Some(last) = prefix.last() {
                            out.push((last.clone(), prefix.clone()));
                        }
                    } else {
                        let mut path = prefix.clone();
                        path.push(name.clone());
                        out.push((name, path));
                    }
                }
                syn::UseTree::Rename(r) => {
                    let mut path = prefix.clone();
                    if r.ident != "self" {
                        path.push(r.ident.to_string());
                    }
                    out.push((r.rename.to_string(), path));
                }
                syn::UseTree::Group(g) => g.items.iter().for_each(|i| walk(i, prefix, out)),
                syn::UseTree::Glob(_) => {}
            }
        }
        let mut out = Vec::new();
        for i in items {
            if let syn::Item::Use(u) = i {
                walk(&u.tree, &mut Vec::new(), &mut out);
            }
        }
        // `use ::kernel::principal` and `use kernel::principal` alike.
        Uses(out)
    }
}

/// `HmacAuth::new(_, f)` calls (any path ending in `HmacAuth::new`, with or
/// without `::<K>`), as `(line, f)`.
struct Finder {
    hits: Vec<(usize, syn::Expr)>,
}

impl<'ast> syn::visit::Visit<'ast> for Finder {
    fn visit_expr_call(&mut self, c: &'ast syn::ExprCall) {
        if let syn::Expr::Path(x) = &*c.func {
            let n = x.path.segments.len();
            if n >= 2 && x.path.segments[n - 1].ident == "new" && x.path.segments[n - 2].ident == "HmacAuth" && c.args.len() == 2 {
                self.hits.push((x.path.segments[n - 1].ident.span().start().line, c.args[1].clone()));
            }
        }
        syn::visit::visit_expr_call(self, c);
    }
}

fn rel(p: &Project, s: &str) -> String {
    s.replace(&format!("{}/", p.root.display()), "")
}

fn rel_path(p: &Project, f: &Path) -> String {
    f.strip_prefix(&p.root).unwrap_or(f).display().to_string()
}

/// The tenant line of a project, from `tenant-of-token`'s status
/// (`proven`, `automation`, `open`, `failed`, `not_run`; `None` when the
/// project does not opt in) and the server's linkage.
pub fn link(p: &Project, obligation: Option<&str>, l: &Linkage) -> TenantLink {
    let ns = p.krate().and_then(|k| crate::util::package_name(&k)).map(|n| crate::util::ident(&n));
    let principal = p.principal_fn().zip(ns.as_deref()).map(|(f, ns)| crate::obligations::principal_lean(ns, f));
    let authenticators = l
        .constructions
        .iter()
        .map(|c| format!("{}: HmacAuth::new(…, {}): {}", c.at, c.what, if c.linked { "linked" } else { "not linked" }))
        .chain(l.custom.iter().map(|c| format!("{c}: impl Authenticator: not linked")))
        .collect();
    let reviewed_note = if l.reviewed.is_empty() {
        String::new()
    } else {
        "; `or_anonymous` and `assume_authenticated` decide a principal without a token and stay reviewed".to_string()
    };
    let (status, summary) = match obligation {
        None => (
            "not_stated",
            "the tenant is decided by the server's principal function and the kernel's tenant, neither stated: it stays \
             in the reviewed surface"
                .to_string(),
        ),
        Some(s @ ("proven" | "automation")) => match l.why_not() {
            None => (
                "proven_linked",
                format!(
                    "covered by tenant-of-token ({s}) and A1 (the token parser is proven); linked is a source check (syn) \
                     that every authenticator is built with {}, not a proof",
                    principal.as_deref().unwrap_or("the principal function")
                ),
            ),
            Some(why) => (
                "proven_not_linked",
                format!("tenant-of-token is {s}, but the server is not linked to it, so the tenant stays reviewed: {why}"),
            ),
        },
        Some(s) => (
            match s {
                "open" => "open",
                "failed" => "failed",
                _ => "not_run",
            },
            format!("tenant-of-token is {s}: the tenant stays in the reviewed surface"),
        ),
    };
    TenantLink {
        status: status.into(),
        obligation: obligation.unwrap_or("not_stated").into(),
        principal,
        authenticators,
        summary: format!("{summary}{reviewed_note}"),
        reviewed: l.reviewed.clone(),
    }
}

/// The per-route line: what decides the tenant a route's commands run in.
pub fn route_line(t: &TenantLink, actor: bool) -> String {
    if !actor {
        return "reviewed: the handler takes no Actor, so it chooses the principal and its tenant".into();
    }
    match t.status.as_str() {
        "proven_linked" => format!(
            "covered: tenant-of-token ({}) with A1; the Actor's principal comes from {} (linked: source check)",
            t.obligation,
            t.principal.as_deref().unwrap_or("the kernel's principal function")
        ),
        _ => format!("reviewed ({}): the server's principal function and the kernel's tenant decide it", t.label()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(toml: &str, server: &[(&str, &str)]) -> (tempfile::TempDir, Project) {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("h5i-app.toml"), toml).unwrap();
        let k = d.path().join("kernel");
        std::fs::create_dir_all(k.join("src")).unwrap();
        std::fs::write(k.join("Cargo.toml"), "[package]\nname = \"forum-kernel\"\nversion = \"0.0.0\"\n").unwrap();
        std::fs::write(k.join("src/lib.rs"), "pub fn principal(org: u64, user: u64) -> P { P { org, user } }\n").unwrap();
        let s = d.path().join("server");
        std::fs::create_dir_all(s.join("src")).unwrap();
        std::fs::write(s.join("Cargo.toml"), "[package]\nname = \"forum-server\"\n[dependencies]\naxum = \"0.8\"\n").unwrap();
        for (f, t) in server {
            std::fs::write(s.join("src").join(f), t).unwrap();
        }
        let p = Project::load(&d.path().join("h5i-app.toml")).unwrap();
        (d, p)
    }

    const OPT_IN: &str = "[extract]\n[authz]\nguarded = [\"guarded_transition\"]\n[obligations]\nprincipal = \"principal\"\n";

    fn linked(main: &str) -> Linkage {
        let (_d, p) = project(OPT_IN, &[("main.rs", main)]);
        linkage(&p)
    }

    #[test]
    fn the_kernel_function_by_path_is_linked() {
        for main in [
            "fn main() { let a = HmacAuth::<Forum>::new(s, forum_kernel::principal); }",
            "use forum_kernel as k;\nfn main() { let a = h5i_app_http::HmacAuth::new(s, k::principal); }",
            "use forum_kernel::{principal, Principal};\nfn main() { let a = HmacAuth::new(s, principal); }",
            "use forum_kernel::principal as mk;\nfn main() { let a = HmacAuth::new(s, mk); }",
            "fn main() { let a = HmacAuth::new(s, |t, u| forum_kernel::principal(t, u)); }",
            "fn main() { let a = HmacAuth::new(s, move |t: u64, u: u64| { forum_kernel::principal(t, u, slug()) }); }",
        ] {
            let l = linked(main);
            assert!(l.linked(), "{main}: {l:?}");
            assert_eq!(l.constructions[0].at, "server/src/main.rs:".to_string() + &(main.lines().count()).to_string());
        }
    }

    #[test]
    fn anything_else_is_not_linked_and_says_why() {
        for (main, why) in [
            ("use forum_server::principal;\nfn main() { HmacAuth::new(s, principal); }", "passes `principal`"),
            ("fn main() { HmacAuth::new(s, |t, u| Principal { org: t, user: u }); }", "closure"),
            ("fn main() { HmacAuth::new(s, |t, u| forum_kernel::principal(u, t)); }", "in order"),
            ("fn main() { HmacAuth::new(s, |t, u| forum_kernel::principal(0, u)); }", "in order"),
            ("fn main() { let f = forum_kernel::principal; HmacAuth::new(s, f); }", "passes `f`"),
            ("fn main() {}", "no `HmacAuth::new`"),
        ] {
            let l = linked(main);
            assert!(!l.linked(), "{main}");
            let w = l.why_not().unwrap();
            assert!(w.contains(why), "{main}: {w}");
        }
        // One construction that is not linked spoils the rest.
        let l = linked("fn a() { HmacAuth::new(s, forum_kernel::principal); }\nfn b() { HmacAuth::new(s, |t, u| P { org: 0, user: u }); }");
        assert!(!l.linked() && l.constructions.len() == 2);
        // A hand-written authenticator, and the calls that stay reviewed.
        let l = linked(
            "fn main() { HmacAuth::new(s, forum_kernel::principal).or_anonymous(p); }\nimpl Authenticator<K> for Mine {}\nfn x() { Actor::assume_authenticated(p); }",
        );
        assert!(!l.linked());
        assert_eq!(l.custom.len(), 1);
        assert_eq!(l.reviewed.len(), 2, "{:?}", l.reviewed);
    }

    #[test]
    fn tests_are_not_read_and_nothing_opts_in_by_default() {
        let l = linked("fn main() { HmacAuth::new(s, forum_kernel::principal); }\n#[cfg(test)]\nmod t { fn f() { HmacAuth::new(s, |t, u| P { org: 0, user: u }); } }");
        assert!(l.linked(), "{l:?}");
        let (_d, p) = project("[extract]\n", &[("main.rs", "fn main() { HmacAuth::new(s, forum_kernel::principal); }")]);
        let l = linkage(&p);
        assert!(!l.linked(), "no `[obligations] principal`: nothing to be linked to");
        let t = link(&p, None, &l);
        assert_eq!(t.status, "not_stated");
    }

    #[test]
    fn the_status_needs_both() {
        let (_d, p) = project(OPT_IN, &[("main.rs", "fn main() { HmacAuth::new(s, forum_kernel::principal); }")]);
        let l = linkage(&p);
        assert_eq!(link(&p, Some("automation"), &l).status, "proven_linked");
        assert_eq!(link(&p, Some("open"), &l).status, "open");
        assert_eq!(link(&p, Some("not_run"), &l).status, "not_run");
        let (_d, p) = project(OPT_IN, &[("main.rs", "fn main() { HmacAuth::new(s, |t, u| P { org: t, user: u }); }")]);
        let t = link(&p, Some("proven"), &linkage(&p));
        assert_eq!(t.label(), "proven, not linked");
        assert!(t.summary.contains("closure"), "{}", t.summary);
        assert!(route_line(&t, true).starts_with("reviewed"));
    }
}
