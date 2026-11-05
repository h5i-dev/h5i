//! `h5i app report`: the per-route guarantee report
//! ([`h5i_app_report::guarantees`]), derived from a project's sources, its
//! `h5i-app.toml` and its recorded `h5i app check` runs.
//!
//! - Routes: `.route("<path>", <method>(handler)…)` calls in each server
//!   crate (the crates `lint` treats as servers), and `rpc_router` /
//!   `executor_rpc_router`, which serve `POST /rpc` with h5i-app-http's own
//!   handler. Routes built any other way (`nest`, a macro, a path that is
//!   not a literal) are missed; a `nest` is reported as an error.
//! - A route's handler takes an `Actor` if one of its parameters' types
//!   names `Actor` (`lint`'s rule, on the syntax tree). Its commands are the
//!   `Command::…` paths in its body; `/rpc` may build every variant of the
//!   kernel's `enum Command`.
//! - A command is read-only if the `read_only` the engine runs (an
//!   `[authz]` app's `View::read_only`, or a `Kernel::read_only` override)
//!   is a `match` whose arm for it is `true`; any other shape is `unknown`.
//! - Theorems: `[check] theorems`, the `[authz]` ones `check` requires, and
//!   for an overridden `Kernel::read_only` the hand-written theorems that
//!   state `ReadOnly.Sound`. Each is `passed` only if the latest recorded
//!   check run passed and gated it; the run's input digest is compared with
//!   the current sources'.
//! - The tenant (`crate::tenant`): `tenant-of-token`'s status as the latest
//!   passed check found it, and whether the server is linked to the
//!   principal function it is proven for, read now from the sources. Each
//!   kernel route's `tenant` line says covered or reviewed.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::Result;
use h5i_app_report::guarantees::{
    self as g, CheckRun, Command, CommandsFrom, Covers, EnginePath, Guarantees, Route, RunStatus, Theorem, TheoremCheck,
    TheoremSource,
};
use h5i_app_report::Run;
use syn::visit::Visit;

use crate::manifest::Project;
use crate::rust::Sources;

/// h5i-app-http's source, for the handler `rpc_router` mounts.
const HTTP_SOURCE: &str = include_str!("../../h5i-app-http/src/lib.rs");

const METHODS: &[&str] = &["get", "post", "put", "delete", "patch", "head", "options", "trace", "any"];

/// The report for `projects`.
pub fn report(projects: &[Project]) -> Guarantees {
    Guarantees::new(projects.iter().map(project).collect())
}

/// One project's part of the report.
pub fn project(p: &Project) -> g::Project {
    let top = crate::util::git_toplevel(&p.root);
    let path = top
        .as_ref()
        .and_then(|t| p.root.strip_prefix(t).ok())
        .map(|r| r.display().to_string())
        .unwrap_or_else(|| p.root.display().to_string());
    let authz = !p.manifest.authz.guarded.is_empty();
    let mut errors = Vec::new();

    let kernel = p.krate().map(|k| Sources::of_crate(&k));
    let variants = match &kernel {
        Some(k) => {
            errors.extend(k.errors.iter().map(|e| rel(p, e)));
            command_variants(k)
        }
        None => Vec::new(),
    };
    let mut servers = Vec::new();
    crate::rust::crates_under(&p.root, &mut servers);
    servers.retain(|c| crate::lint::is_server(c));
    let server_sources: Vec<Sources> = servers.iter().map(|s| Sources::of_crate(s)).collect();
    for s in &server_sources {
        errors.extend(s.errors.iter().map(|e| rel(p, e)));
    }

    // Which commands run on the read-only path.
    let overridden = authz || server_sources.iter().any(|s| s.impls_of("Kernel").iter().any(|i| i.fns.contains("read_only")));
    let read_only: Option<BTreeSet<String>> = if overridden {
        kernel.iter().chain(&server_sources).find_map(read_only_arms)
    } else {
        Some(BTreeSet::new())
    };
    if overridden && read_only.is_none() {
        errors.push("`read_only` is overridden, but not as a `match` with `true`/`false` arms: engine paths unknown".into());
    }
    let engine_path = |name: &str| match &read_only {
        Some(set) if set.contains(name) => EnginePath::ReadOnly,
        Some(_) => EnginePath::Write,
        None => EnginePath::Unknown,
    };

    let theorems = theorems(p, authz, overridden, &mut errors);
    let all_cmds: Vec<String> = theorems.iter().filter(|t| t.covers == Covers::AllCommands).map(|t| t.name.clone()).collect();
    let ro_cmds: Vec<String> = theorems.iter().filter(|t| t.covers == Covers::ReadOnlyCommands).map(|t| t.name.clone()).collect();

    let check = latest_check(p, top.as_deref());
    let obligations = obligations(p, &check, &mut errors);
    // The tenant: `tenant-of-token` as the latest passed check found it, and
    // the server's linkage, read now from the sources.
    let tenant = p.krate().map(|_| {
        let status = p
            .principal_fn()
            .map(|_| {
                obligations
                    .iter()
                    .find(|o| o.name == crate::obligations::TENANT_OF_TOKEN)
                    .map_or("not_run", |o| o.status.as_str())
            });
        crate::tenant::link(p, status, &crate::tenant::linkage(p))
    });

    let mut routes = Vec::new();
    for s in &server_sources {
        for found in find_routes(s, &mut errors, p) {
            let (actor, kernel_route, commands, from) = match &found.handler {
                Handler::Rpc => {
                    let (actor, builds) = rpc_handler();
                    (Some(actor), builds, variants.clone(), CommandsFrom::EveryVariant)
                }
                Handler::Named(name) => match find_fn(&server_sources, name) {
                    Some(f) => {
                        let cmds = command_paths(&f.block);
                        let reaches = !cmds.is_empty() || calls_engine(&f.block);
                        (Some(takes_actor(&f.sig)), reaches, cmds, if reaches { CommandsFrom::HandlerBody } else { CommandsFrom::None })
                    }
                    None => {
                        errors.push(format!("{}: handler `{name}` not found in the server's sources", found.at));
                        (None, false, Vec::new(), CommandsFrom::None)
                    }
                },
                Handler::Closure(c) => {
                    let cmds = command_paths(&c.body);
                    let reaches = !cmds.is_empty() || calls_engine(&c.body);
                    let actor = c.inputs.iter().any(|i| matches!(i, syn::Pat::Type(t) if type_names_actor(&t.ty)));
                    (Some(actor), reaches, cmds, if reaches { CommandsFrom::HandlerBody } else { CommandsFrom::None })
                }
            };
            let commands: Vec<Command> = commands.iter().map(|c| Command { name: c.clone(), path: engine_path(c) }).collect();
            let mut covered = Vec::new();
            let mut assumptions = Vec::new();
            let mut not_verified = Vec::new();
            if kernel_route {
                covered.extend(all_cmds.iter().cloned());
                if commands.iter().any(|c| c.path != EnginePath::Write) {
                    covered.extend(ro_cmds.iter().cloned());
                }
                if actor == Some(true) {
                    assumptions.push("A1");
                } else {
                    not_verified.push(g::NO_ACTOR.to_string());
                }
                assumptions.extend(["A2", "A3", "A4", "A5", "A6", "A7", "A8", "A9", "A10"]);
                if authz {
                    assumptions.push("A11");
                }
                not_verified.extend(g::KERNEL_ROUTE_NOT_VERIFIED.iter().map(|s| s.to_string()));
            } else {
                not_verified.extend(g::OUTSIDE_KERNEL.iter().map(|s| s.to_string()));
            }
            routes.push(Route {
                method: found.method,
                path: found.path,
                at: found.at,
                handler: found.label,
                actor,
                kernel: kernel_route,
                commands,
                commands_from: from,
                theorems: covered,
                assumptions: assumptions.into_iter().map(String::from).collect(),
                not_verified,
                tenant: tenant.as_ref().filter(|_| kernel_route).map(|t| crate::tenant::route_line(t, actor == Some(true))),
            });
        }
    }
    routes.sort_by(|a, b| (&a.path, &a.method).cmp(&(&b.path, &b.method)));

    let theorems = theorems.into_iter().map(|t| with_check(t, &check)).collect();
    g::Project {
        path,
        kernel: p.manifest.extract.as_ref().map(|e| e.krate.clone()),
        authz,
        check: check.as_ref().map(|(run, current)| CheckRun { run: Some(run.id.clone()), status: status(&run.status), inputs_current: *current }).unwrap_or(CheckRun {
            run: None,
            status: RunStatus::NotRun,
            inputs_current: false,
        }),
        theorems,
        routes,
        errors,
        obligations,
        tenant,
    }
}

/// The project's framework-defined obligations (`crate::obligations`), each
/// with what the latest check run found if it passed on the current inputs
/// (else `not_run`), and those not stated, with why.
fn obligations(p: &Project, check: &Option<(Run, bool)>, errors: &mut Vec<String>) -> Vec<h5i_app_report::ObligationResult> {
    let kernel = match crate::obligations::Kernel::read(p) {
        Ok(Some(k)) => k,
        Ok(None) => return Vec::new(),
        Err(e) => {
            errors.push(format!("the obligations: {e:#}"));
            return Vec::new();
        }
    };
    let recorded: &[h5i_app_report::ObligationResult] = match check {
        Some((run, true)) if run.status == "passed" => &run.obligations,
        _ => &[],
    };
    let mut out: Vec<h5i_app_report::ObligationResult> = crate::obligations::of_kernel(p, &kernel)
        .into_iter()
        .map(|o| match recorded.iter().find(|r| r.name == o.name && r.statement == o.statement) {
            Some(r) => r.clone(),
            None => h5i_app_report::ObligationResult {
                name: o.name.into(),
                statement: o.statement,
                status: "not_run".into(),
                theorem: o.theorem,
                note: o.note,
            },
        })
        .collect();
    for (name, why) in crate::obligations::not_stated(p, &kernel) {
        if !out.iter().any(|o| o.name == name) {
            out.push(h5i_app_report::ObligationResult {
                name: name.into(),
                statement: String::new(),
                status: "not_stated".into(),
                theorem: None,
                note: Some(why),
            });
        }
    }
    out
}

fn rel(p: &Project, s: &str) -> String {
    s.replace(&format!("{}/", p.root.display()), "")
}

fn rel_path(p: &Project, f: &Path) -> String {
    f.strip_prefix(&p.root).unwrap_or(f).display().to_string()
}

/// The variants of the kernel's `enum Command`, in declaration order.
fn command_variants(k: &Sources) -> Vec<String> {
    k.files
        .iter()
        .flat_map(|(_, _, items)| items)
        .find_map(|i| match i {
            syn::Item::Enum(e) if e.ident == "Command" => Some(e.variants.iter().map(|v| v.ident.to_string()).collect()),
            _ => None,
        })
        .unwrap_or_default()
}

/// The variants a `fn read_only` (free or in an impl) maps to `true`, if it
/// is one `match` whose arms are `Command::…` patterns or `_`, each with a
/// `true` or `false` literal (`_ => true` excluded), or one
/// `matches!(cmd, Command::… | …)` without a guard; `None` for any other
/// shape.
fn read_only_arms(s: &Sources) -> Option<BTreeSet<String>> {
    let mut fns: Vec<&syn::Block> = Vec::new();
    for (_, _, items) in &s.files {
        for i in items {
            match i {
                syn::Item::Fn(f) if f.sig.ident == "read_only" => fns.push(&*f.block),
                syn::Item::Impl(imp) => {
                    for it in &imp.items {
                        if let syn::ImplItem::Fn(f) = it
                            && f.sig.ident == "read_only"
                        {
                            fns.push(&f.block);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    fns.into_iter().find_map(|b| {
        // `matches!(cmd, A | B)`.
        if let Some(mac) = single_macro(b)
            && mac.path.is_ident("matches")
        {
            let parsed = mac.parse_body_with(|input: syn::parse::ParseStream| {
                let _: syn::Expr = input.parse()?;
                let _: syn::Token![,] = input.parse()?;
                let pat = syn::Pat::parse_multi_with_leading_vert(input)?;
                let _: Option<syn::Token![,]> = input.parse()?;
                if !input.is_empty() {
                    return Err(input.error("a guard or more"));
                }
                Ok(pat)
            });
            let mut names = Vec::new();
            return match parsed {
                Ok(pat) if pattern_variants(&pat, &mut names) && !names.is_empty() => Some(names.into_iter().collect()),
                _ => None,
            };
        }
        let [syn::Stmt::Expr(syn::Expr::Match(m), None)] = b.stmts.as_slice() else { return None };
        let mut yes = BTreeSet::new();
        for arm in &m.arms {
            let val = match &*arm.body {
                syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Bool(b), .. }) => b.value,
                _ => return None,
            };
            let mut names = Vec::new();
            if !pattern_variants(&arm.pat, &mut names) || arm.guard.is_some() {
                return None;
            }
            match (names.is_empty(), val) {
                // `_ => true` would make commands read-only that are not named here.
                (true, true) => return None,
                (_, true) => yes.extend(names),
                (_, false) => {}
            }
        }
        Some(yes)
    })
}

/// The macro call that is a block's only statement and its value.
fn single_macro(b: &syn::Block) -> Option<&syn::Macro> {
    match b.stmts.as_slice() {
        [syn::Stmt::Expr(syn::Expr::Macro(m), None)] => Some(&m.mac),
        [syn::Stmt::Macro(m)] if m.semi_token.is_none() => Some(&m.mac),
        _ => None,
    }
}

/// `Command::X`, `Command::X { .. }`, `Command::X(..)`, `A | B` and `_`;
/// false for anything else.
fn pattern_variants(p: &syn::Pat, out: &mut Vec<String>) -> bool {
    let last = |path: &syn::Path| path.segments.last().map(|s| s.ident.to_string());
    match p {
        syn::Pat::Wild(_) => true,
        syn::Pat::Or(o) => o.cases.iter().all(|c| pattern_variants(c, out)),
        syn::Pat::Path(x) => last(&x.path).map(|n| out.push(n)).is_some(),
        syn::Pat::Struct(x) => last(&x.path).map(|n| out.push(n)).is_some(),
        syn::Pat::TupleStruct(x) => last(&x.path).map(|n| out.push(n)).is_some(),
        _ => false,
    }
}

/// Whether the framework's `rpc` handler takes an `Actor`, and whether it
/// runs a decoded command (`decode_command` then `respond`), read off
/// h5i-app-http's source.
fn rpc_handler() -> (bool, bool) {
    let Ok(file) = syn::parse_file(HTTP_SOURCE) else { return (false, false) };
    file.items
        .iter()
        .find_map(|i| match i {
            syn::Item::Fn(f) if f.sig.ident == "rpc" => {
                let text = quote::quote!(#f).to_string();
                Some((takes_actor(&f.sig), text.contains("decode_command") && text.contains("respond")))
            }
            _ => None,
        })
        .unwrap_or((false, false))
}

enum Handler {
    /// h5i-app-http's `rpc`, mounted by `rpc_router`.
    Rpc,
    Named(String),
    Closure(Box<syn::ExprClosure>),
}

struct Found {
    method: String,
    path: String,
    at: String,
    label: String,
    handler: Handler,
}

/// Every route the server's sources register.
fn find_routes(s: &Sources, errors: &mut Vec<String>, p: &Project) -> Vec<Found> {
    struct V<'a> {
        file: &'a Path,
        p: &'a Project,
        out: Vec<Found>,
        errors: Vec<String>,
    }
    impl V<'_> {
        fn at(&self, line: usize) -> String {
            format!("{}:{line}", rel_path(self.p, self.file))
        }
    }
    impl<'ast> Visit<'ast> for V<'_> {
        fn visit_expr_method_call(&mut self, c: &'ast syn::ExprMethodCall) {
            let m = c.method.to_string();
            let line = c.method.span().start().line;
            if m == "route" && c.args.len() == 2 {
                match &c.args[0] {
                    syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(path), .. }) => {
                        let mut handlers = Vec::new();
                        method_router(&c.args[1], &mut handlers);
                        if handlers.is_empty() {
                            self.errors.push(format!("{}: route {:?} with a method router this report cannot read", self.at(line), path.value()));
                        }
                        for (method, handler) in handlers {
                            let (label, handler) = match handler {
                                syn::Expr::Path(x) => {
                                    let label = quote::quote!(#x).to_string().replace(' ', "");
                                    let name = x.path.segments.last().map(|s| s.ident.to_string()).unwrap_or_default();
                                    (label, Handler::Named(name))
                                }
                                syn::Expr::Closure(cl) => ("closure".to_string(), Handler::Closure(Box::new(cl.clone()))),
                                other => {
                                    self.errors.push(format!("{}: handler `{}` is not a path or closure", self.at(line), quote::quote!(#other)));
                                    continue;
                                }
                            };
                            self.out.push(Found { method: method.to_uppercase(), path: path.value(), at: self.at(line), label, handler });
                        }
                    }
                    other => self.errors.push(format!("{}: route path `{}` is not a string literal", self.at(line), quote::quote!(#other))),
                }
            } else if m == "nest" || m == "nest_service" || m == "route_service" || m == "fallback" {
                self.errors.push(format!("{}: `.{m}(…)` is not analyzed; the routes it adds are missing", self.at(line)));
            }
            syn::visit::visit_expr_method_call(self, c);
        }
        fn visit_expr_call(&mut self, c: &'ast syn::ExprCall) {
            if let syn::Expr::Path(x) = &*c.func
                && let Some(last) = x.path.segments.last()
                && (last.ident == "rpc_router" || last.ident == "executor_rpc_router")
            {
                let line = last.ident.span().start().line;
                self.out.push(Found {
                    method: "POST".into(),
                    path: "/rpc".into(),
                    at: self.at(line),
                    label: "h5i_app_http::rpc".into(),
                    handler: Handler::Rpc,
                });
            }
            syn::visit::visit_expr_call(self, c);
        }
    }
    let mut out = Vec::new();
    for (file, _, items) in &s.files {
        let mut v = V { file, p, out: Vec::new(), errors: Vec::new() };
        for i in items {
            // `rpc_router`'s own definition is not a registration.
            if let syn::Item::Fn(f) = i
                && (f.sig.ident == "rpc_router" || f.sig.ident == "executor_rpc_router")
            {
                continue;
            }
            v.visit_item(i);
        }
        out.extend(v.out);
        errors.extend(v.errors);
    }
    out
}

/// `get(h)`, `post(h).put(h2)`, `axum::routing::get(h)`: `(method, handler)`.
fn method_router<'a>(e: &'a syn::Expr, out: &mut Vec<(String, &'a syn::Expr)>) {
    match e {
        syn::Expr::Call(c) => {
            if let syn::Expr::Path(x) = &*c.func
                && let Some(last) = x.path.segments.last()
                && METHODS.contains(&last.ident.to_string().as_str())
                && c.args.len() == 1
            {
                out.push((last.ident.to_string(), &c.args[0]));
            }
        }
        syn::Expr::MethodCall(c) => {
            method_router(&c.receiver, out);
            if METHODS.contains(&c.method.to_string().as_str()) && c.args.len() == 1 {
                out.push((c.method.to_string(), &c.args[0]));
            }
        }
        _ => {}
    }
}

/// The free or associated function `name` in the server sources.
fn find_fn(sources: &[Sources], name: &str) -> Option<syn::ItemFn> {
    for s in sources {
        for (_, _, items) in &s.files {
            for i in items {
                match i {
                    syn::Item::Fn(f) if f.sig.ident == name => return Some(f.clone()),
                    syn::Item::Impl(imp) => {
                        for it in &imp.items {
                            if let syn::ImplItem::Fn(f) = it
                                && f.sig.ident == name
                            {
                                return Some(syn::ItemFn { attrs: f.attrs.clone(), vis: f.vis.clone(), sig: f.sig.clone(), block: Box::new(f.block.clone()) });
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    None
}

fn type_names_actor(t: &syn::Type) -> bool {
    struct V(bool);
    impl<'ast> Visit<'ast> for V {
        fn visit_path_segment(&mut self, s: &'ast syn::PathSegment) {
            self.0 |= s.ident == "Actor";
            syn::visit::visit_path_segment(self, s);
        }
    }
    let mut v = V(false);
    v.visit_type(t);
    v.0
}

fn takes_actor(sig: &syn::Signature) -> bool {
    sig.inputs.iter().any(|i| matches!(i, syn::FnArg::Typed(t) if type_names_actor(&t.ty)))
}

/// `Command::X` paths (expressions or patterns), in first-seen order.
fn command_paths<T: ToTokensVisit>(node: &T) -> Vec<String> {
    struct V(Vec<String>);
    impl<'ast> Visit<'ast> for V {
        fn visit_path(&mut self, p: &'ast syn::Path) {
            let n = p.segments.len();
            if n >= 2 && p.segments[n - 2].ident == "Command" {
                let v = p.segments[n - 1].ident.to_string();
                if !self.0.contains(&v) {
                    self.0.push(v);
                }
            }
            syn::visit::visit_path(self, p);
        }
    }
    let mut v = V(Vec::new());
    node.visit_with(&mut v);
    v.0
}

/// Calls the engine: `respond`, `run`, `execute…`.
fn calls_engine<T: ToTokensVisit>(node: &T) -> bool {
    struct V(bool);
    impl<'ast> Visit<'ast> for V {
        fn visit_expr_method_call(&mut self, c: &'ast syn::ExprMethodCall) {
            let m = c.method.to_string();
            self.0 |= m == "respond" || m == "run" || m.starts_with("execute");
            syn::visit::visit_expr_method_call(self, c);
        }
    }
    let mut v = V(false);
    node.visit_with(&mut v);
    v.0
}

/// A node a visitor can walk.
trait ToTokensVisit {
    fn visit_with<'ast, V: Visit<'ast>>(&'ast self, v: &mut V);
}

impl ToTokensVisit for syn::Block {
    fn visit_with<'ast, V: Visit<'ast>>(&'ast self, v: &mut V) {
        v.visit_block(self);
    }
}

impl ToTokensVisit for Box<syn::Block> {
    fn visit_with<'ast, V: Visit<'ast>>(&'ast self, v: &mut V) {
        v.visit_block(self);
    }
}

impl ToTokensVisit for Box<syn::Expr> {
    fn visit_with<'ast, V: Visit<'ast>>(&'ast self, v: &mut V) {
        v.visit_expr(self);
    }
}

/// The theorems a check run of `p` requires or gates for its commands.
fn theorems(p: &Project, authz: bool, read_only_overridden: bool, errors: &mut Vec<String>) -> Vec<Theorem> {
    let lean = lean_files(&p.proofs());
    let mut out: Vec<Theorem> = Vec::new();
    let mut push = |name: String, source, covers| {
        if !out.iter().any(|t: &Theorem| t.name == name) {
            let file = lean_file_of(p, &lean, &name);
            out.push(Theorem { name, source, file, covers, check: TheoremCheck::NotRun });
        }
    };
    // `lint` requires a hand-written proof stating `ReadOnly.Sound` for an
    // overridden `Kernel::read_only`; it covers the read-only commands.
    let read_only = if !authz && read_only_overridden { read_only_theorems(&p.proofs()) } else { Vec::new() };
    if !authz && read_only_overridden && read_only.is_empty() {
        errors.push("`Kernel::read_only` is overridden, but no hand-written theorem states `ReadOnly.Sound`".into());
    }
    for t in &p.manifest.check.theorems {
        let covers = if read_only.contains(t) { Covers::ReadOnlyCommands } else { Covers::AllCommands };
        push(t.clone(), TheoremSource::CheckTheorems, covers);
    }
    if authz {
        match crate::check::authz_theorems(p) {
            Ok(ts) => {
                for t in ts {
                    let covers = if t.ends_with(".read_only_sound") { Covers::ReadOnlyCommands } else { Covers::AllCommands };
                    push(t, TheoremSource::Authz, covers);
                }
            }
            Err(e) => errors.push(format!("the [authz] theorems: {e:#}")),
        }
    }
    for n in read_only {
        push(n, TheoremSource::ReadOnlyProof, Covers::ReadOnlyCommands);
    }
    // Theorems named for a framework-defined obligation (or generated for one).
    if let Ok(obligations) = crate::obligations::of(p) {
        for o in obligations {
            if let Some(t) = o.theorem {
                let covers = if o.name == "read-only" { Covers::ReadOnlyCommands } else { Covers::AllCommands };
                push(t, TheoremSource::Obligation, covers);
            }
        }
    }
    out
}

/// `.lean` files under the proofs, outside Lake's build directories.
fn lean_files(dir: &Path) -> Vec<PathBuf> {
    fn walk(d: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(d) else { return };
        for e in entries.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if p.is_dir() {
                if !name.starts_with('.') && name != "build" {
                    walk(&p, out);
                }
            } else if p.extension().is_some_and(|x| x == "lean") {
                out.push(p);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, &mut out);
    out.sort();
    out
}

/// The file stating `name`: the one declaring the longest dotted suffix of
/// it as a `theorem`, or for `[authz]` theorems the file running `h5i_authz`
/// on the guarded function (they are generated by that command).
fn lean_file_of(p: &Project, files: &[PathBuf], name: &str) -> Option<String> {
    let parts: Vec<&str> = name.split('.').collect();
    let mut best: Option<(usize, &PathBuf)> = None;
    for f in files {
        let code = crate::check::lean_code(&std::fs::read_to_string(f).unwrap_or_default());
        for k in (1..=parts.len()).rev() {
            let suffix = parts[parts.len() - k..].join(".");
            let declared = code.lines().any(|l| {
                let mut w = l.split_whitespace();
                matches!(w.next(), Some("theorem" | "lemma")) && w.next() == Some(suffix.as_str())
            });
            let generated = k >= 2
                && matches!(parts[parts.len() - 1], "authorized" | "kernel_apply_eq" | "read_only_sound")
                && code.lines().any(|l| l.trim_start().starts_with("h5i_authz") && l.contains(&parts[..parts.len() - 1].join(".")));
            if declared || generated {
                if best.is_none_or(|(b, _)| k > b) {
                    best = Some((k, f));
                }
                break;
            }
        }
    }
    best.map(|(_, f)| rel_path(p, f))
}

/// Theorems in hand-written proof files (not `generated/`) whose statement
/// mentions `ReadOnly.Sound`, by full name (`namespace … end` followed).
fn read_only_theorems(proofs: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for f in lean_files(proofs) {
        if f.strip_prefix(proofs).is_ok_and(|r| r.starts_with("generated")) {
            continue;
        }
        let code = crate::check::lean_code(&std::fs::read_to_string(&f).unwrap_or_default());
        let mut ns: Vec<String> = Vec::new();
        let mut current: Option<String> = None;
        for l in code.lines() {
            let mut w = l.split_whitespace();
            match w.next() {
                Some("namespace") => ns.extend(w.next().map(String::from)),
                Some("end") => {
                    if let Some(n) = w.next()
                        && ns.last().is_some_and(|last| last == n)
                    {
                        ns.pop();
                    }
                }
                Some("theorem" | "lemma") => {
                    current = w.next().map(|n| ns.iter().map(String::as_str).chain([n]).collect::<Vec<_>>().join("."));
                }
                _ => {}
            }
            if l.contains("ReadOnly.Sound")
                && let Some(n) = current.take()
            {
                out.push(n);
            }
        }
    }
    out
}

/// The latest recorded check run, and whether its inputs are the current ones.
fn latest_check(p: &Project, top: Option<&Path>) -> Option<(Run, bool)> {
    let dir = p.root.join(h5i_app_report::RUNS);
    let mut runs: Vec<Run> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| h5i_app_report::read_text(&e.path().join("run.json")).ok())
        .filter_map(|t| serde_json::from_str::<Run>(&t).ok())
        .filter(|r| r.kind == "check")
        .collect();
    runs.sort_by(|a, b| (a.started, &a.id).cmp(&(b.started, &b.id)));
    let run = runs.pop()?;
    let current = h5i_app_report::inputs(top.unwrap_or(&p.root));
    let same = current.digest == run.inputs.digest && current.errors.is_empty() && run.inputs.errors.is_empty();
    Some((run, same))
}

fn status(s: &str) -> RunStatus {
    match s {
        "passed" => RunStatus::Passed,
        "failed" => RunStatus::Failed,
        "running" => RunStatus::Running,
        "interrupted" => RunStatus::Interrupted,
        _ => RunStatus::Other,
    }
}

/// What the run observed for `t`: a passed run gated it if it required it
/// by name (the gate fails a run whose required theorem is missing or uses
/// another axiom), or catalogued it on the standard axioms only.
fn with_check(mut t: Theorem, check: &Option<(Run, bool)>) -> Theorem {
    t.check = match check {
        None => TheoremCheck::NotRun,
        Some((run, _)) if run.status != "passed" => TheoremCheck::RunNotPassed,
        Some((run, current)) => {
            let required = run.required_theorems.contains(&t.name);
            let catalogued = run
                .declarations
                .iter()
                .any(|d| d.name == t.name && d.kind == "theorem" && d.axioms.iter().all(|a| crate::check::STANDARD_AXIOMS.contains(&a.as_str())));
            match (required || catalogued, current) {
                (false, _) => TheoremCheck::NotGated,
                (true, true) => TheoremCheck::Passed,
                (true, false) => TheoremCheck::PassedOnOtherInputs,
            }
        }
    };
    t
}

/// `h5i app report` on `projects`: JSON or text on stdout.
pub fn print(projects: &[Project], json: bool) -> Result<()> {
    let r = report(projects);
    if json {
        println!("{}", serde_json::to_string_pretty(&r)?);
    } else {
        print!("{}", r.render());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example(name: &str) -> Project {
        let f = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/app").join(name).join("h5i-app.toml");
        Project::load(&f).unwrap()
    }

    fn route<'a>(p: &'a g::Project, method: &str, path: &str) -> &'a Route {
        p.routes.iter().find(|r| r.method == method && r.path == path).unwrap_or_else(|| panic!("no {method} {path}: {:?}", p.routes))
    }

    fn names(cs: &[Command], path: EnginePath) -> Vec<&str> {
        cs.iter().filter(|c| c.path == path).map(|c| c.name.as_str()).collect()
    }

    #[test]
    fn ledger() {
        let r = project(&example("ledger"));
        assert_eq!(r.path, "examples/app/ledger");
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert!(!r.authz);
        assert_eq!(r.routes.len(), 2, "{:?}", r.routes);
        let health = route(&r, "GET", "/healthz");
        assert!(!health.kernel && health.commands.is_empty() && health.theorems.is_empty() && health.assumptions.is_empty());
        let rpc = route(&r, "POST", "/rpc");
        assert_eq!((rpc.actor, rpc.kernel, rpc.commands_from), (Some(true), true, CommandsFrom::EveryVariant));
        // No `read_only` override: every command, `List` too, takes the write path.
        assert_eq!(names(&rpc.commands, EnginePath::Write), ["Open", "Deposit", "Withdraw", "Transfer", "List"]);
        let t = &r.theorems[0];
        assert_eq!((t.name.as_str(), t.source, t.covers), ("ledger_kernel.Storage.db_inv", TheoremSource::CheckTheorems, Covers::AllCommands));
        assert_eq!(t.file.as_deref(), Some("proofs/Storage.lean"));
        assert_eq!(rpc.theorems, ["ledger_kernel.Storage.db_inv", "ledger_kernel.Theorems.panic_free"]);
        let pf = &r.theorems[1];
        assert_eq!((pf.source, pf.file.as_deref()), (TheoremSource::Obligation, Some("proofs/Theorems.lean")));
        assert_eq!(rpc.assumptions, ["A1", "A2", "A3", "A4", "A5", "A6", "A7", "A8", "A9", "A10"]);
        assert!(rpc.not_verified.iter().any(|n| n.contains("request decoding")));
        // Not opted in to `tenant-of-token`: not stated, and the tenant stays reviewed.
        let t = r.tenant.as_ref().unwrap();
        assert_eq!((t.status.as_str(), t.obligation.as_str()), ("not_stated", "not_stated"));
        assert!(r.obligations.iter().any(|o| o.name == "tenant-of-token" && o.status == "not_stated"));
        assert!(rpc.tenant.as_deref().unwrap().starts_with("reviewed (not_stated)"), "{:?}", rpc.tenant);
    }

    #[test]
    fn forum_authz() {
        let r = project(&example("forum"));
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert!(r.authz);
        let rpc = route(&r, "POST", "/rpc");
        assert_eq!(names(&rpc.commands, EnginePath::ReadOnly), ["ReadPost", "ListPosts", "ReadUser"]);
        assert_eq!(names(&rpc.commands, EnginePath::Write), ["Register", "CreatePost", "EditPost", "SetPublished", "DeletePost", "SetAdmin"]);
        let want = [
            ("forum_kernel.guarded_transition.authorized", Covers::AllCommands),
            ("forum_kernel.guarded_transition.kernel_apply_eq", Covers::AllCommands),
            ("forum_kernel.guarded_transition.read_only_sound", Covers::ReadOnlyCommands),
            ("forum_kernel.Committed.sql_fits", Covers::AllCommands),
            ("forum_kernel.Committed.apply_enc", Covers::AllCommands),
            ("forum_kernel.Committed.pg_stored", Covers::AllCommands),
            ("forum_kernel.Committed.storage", Covers::AllCommands),
        ];
        let got: Vec<(&str, Covers)> = r.theorems.iter().map(|t| (t.name.as_str(), t.covers)).collect();
        assert_eq!(got, want);
        for t in &r.theorems {
            let source = if t.name.ends_with(".storage") { TheoremSource::Obligation } else { TheoremSource::Authz };
            assert_eq!((t.source, t.file.as_deref()), (source, Some("proofs/generated/Authz.lean")), "{}", t.name);
        }
        assert_eq!(rpc.theorems.len(), 7);
        // The framework-defined obligations, with nothing recorded yet for them.
        let names: Vec<&str> = r.obligations.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(names, ["authz", "read-only", "storage", "panic-free", "tenant-isolation", "tenant-of-token", "reply-codec"]);
        assert_eq!(r.obligations.last().map(|o| o.status.as_str()), Some("not_stated"));
        assert_eq!(rpc.assumptions.last().map(String::as_str), Some("A11"));
        // Opted in to `tenant-of-token` and linked: `main.rs` gives `HmacAuth::new`
        // the kernel's `principal`. Without a passed run on these inputs the
        // obligation is `not_run`, so the tenant is not yet covered.
        let t = r.tenant.as_ref().unwrap();
        assert_eq!(t.principal.as_deref(), Some("forum_kernel.principal"));
        assert_eq!(t.authenticators.len(), 1);
        assert!(t.authenticators[0].ends_with("HmacAuth::new(…, forum_kernel::principal): linked"), "{:?}", t.authenticators);
        assert!(rpc.tenant.as_deref().is_some_and(|l| l.starts_with("reviewed") || l.starts_with("covered")), "{:?}", rpc.tenant);
        assert!(route(&r, "GET", "/healthz").tenant.is_none(), "no kernel command, no tenant");
        // The report and its schema agree, and the text form names each route.
        let all = report(&[example("ledger"), example("forum")]);
        let text = serde_json::to_string(&all).unwrap();
        assert_eq!(Guarantees::parse(&text).unwrap(), all);
        let shown = all.render();
        assert!(shown.contains("examples/app/forum [authz]") && shown.contains("ReadPost (read-only)"), "{shown}");
    }

    #[test]
    fn calculator_read_only_proof() {
        let r = project(&example("calculator"));
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        let rpc = route(&r, "POST", "/rpc");
        assert_eq!(names(&rpc.commands, EnginePath::ReadOnly), ["Get"]);
        // Listed in `[check] theorems` and found stating `ReadOnly.Sound`: once, for read-only commands.
        let ro: Vec<_> = r.theorems.iter().filter(|t| t.name.ends_with("ReadOnly.sound")).collect();
        assert_eq!(ro.len(), 1, "{:?}", r.theorems);
        assert_eq!((ro[0].source, ro[0].covers), (TheoremSource::CheckTheorems, Covers::ReadOnlyCommands));
    }

    /// A project with its own routes, a `read_only` override and its proof,
    /// and a recorded check run.
    fn custom(d: &Path) -> Project {
        let w = |f: &str, t: &str| {
            let p = d.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, t).unwrap();
        };
        w("h5i-app.toml", "[extract]\n[check]\ntheorems = [\"k.Storage.db_inv\"]\n");
        w("kernel/Cargo.toml", "[package]\nname = \"k\"\n");
        w("kernel/src/lib.rs", "pub enum Command { Approve { doc: u64 }, Read, Wipe }\npub fn read_only(c: &Command) -> bool {\n    match c { Command::Read => true, _ => false }\n}\n");
        w("server/Cargo.toml", "[package]\nname = \"s\"\n[dependencies]\naxum = \"0.8\"\n");
        w(
            "server/src/main.rs",
            "fn main() {\n    let r = Router::new()\n        .route(\"/docs/{id}/approve\", post(approve).get(read))\n        .route(\"/wipe\", delete(handlers::wipe))\n        .nest(\"/admin\", admin());\n}\n\
             async fn approve(State(app): State<A>, actor: Actor<K>, Path(doc): Path<u64>, h: HeaderMap) -> Response {\n    app.respond(&actor, Command::Approve { doc }, &h).await\n}\n\
             async fn read(State(app): State<A>, actor: Actor<K>, h: HeaderMap) -> Response { app.respond(&actor, k::Command::Read, &h).await }\n\
             mod handlers { pub async fn wipe(State(app): State<A>, h: HeaderMap) -> Response { app.respond(&Actor::assume_authenticated(root()), Command::Wipe, &h).await } }\n\
             impl Kernel for K { fn read_only(c: &Command) -> bool { k::read_only(c) } }\n",
        );
        w("proofs/Storage.lean", "theorem db_inv : True := trivial\n");
        w("proofs/ReadOnly.lean", "-- theorem decoy : ReadOnly.Sound\nnamespace k\ntheorem ro_sound : H5iAppLib.ReadOnly.Sound k.transition k.read_only := k_sound\nend k\n");
        Project::load(&d.join("h5i-app.toml")).unwrap()
    }

    fn record(d: &Path, id: &str, started: u64, status: &str, required: &[&str], inputs: h5i_app_report::Inputs) {
        let run = serde_json::json!({
            "version": 1, "id": id, "kind": "check", "started": started, "finished": started, "status": status,
            "inputs": inputs, "inputs_changed": false, "targets": [], "toolchain": {}, "baseline": "not_run",
            "trials": [], "stages": [], "unbuilt_modules": [], "required_theorems": required, "error": null,
            "declarations": [{"name": "k.ro_sound", "kind": "theorem", "signature": "", "dependencies": [], "axioms": ["propext"]}],
        });
        let dir = d.join(h5i_app_report::RUNS).join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("run.json"), run.to_string()).unwrap();
    }

    #[test]
    fn custom_routes_read_only_and_check_runs() {
        let d = tempfile::tempdir().unwrap();
        let p = custom(d.path());
        let r = project(&p);
        assert_eq!(r.errors.len(), 1, "{:?}", r.errors);
        assert!(r.errors[0].contains(".nest(") && r.errors[0].starts_with("server/src/main.rs:5"), "{:?}", r.errors);
        let approve = route(&r, "POST", "/docs/{id}/approve");
        assert_eq!((approve.actor, approve.commands_from), (Some(true), CommandsFrom::HandlerBody));
        assert_eq!(names(&approve.commands, EnginePath::Write), ["Approve"]);
        assert_eq!(approve.at, "server/src/main.rs:3");
        // Only routes with a read-only command are covered by the read-only proof.
        assert_eq!(approve.theorems, ["k.Storage.db_inv"]);
        let read = route(&r, "GET", "/docs/{id}/approve");
        assert_eq!(names(&read.commands, EnginePath::ReadOnly), ["Read"]);
        assert_eq!(read.theorems, ["k.Storage.db_inv", "k.ro_sound"]);
        let wipe = route(&r, "DELETE", "/wipe");
        assert_eq!((wipe.actor, wipe.kernel, wipe.handler.as_str()), (Some(false), true, "handlers::wipe"));
        assert!(!wipe.assumptions.contains(&"A1".to_string()));
        assert_eq!(wipe.not_verified[0], g::NO_ACTOR);
        let ro = r.theorems.iter().find(|t| t.name == "k.ro_sound").unwrap();
        assert_eq!((ro.source, ro.covers, ro.file.as_deref()), (TheoremSource::ReadOnlyProof, Covers::ReadOnlyCommands, Some("proofs/ReadOnly.lean")));
        assert_eq!(r.theorems.len(), 2, "the commented-out decoy is not a theorem");
        assert!(r.theorems.iter().all(|t| t.check == TheoremCheck::NotRun));
        assert_eq!(r.check.status, RunStatus::NotRun);

        let checks = |p: &Project| {
            let r = project(p);
            (r.check.status, r.theorems.iter().map(|t| t.check).collect::<Vec<_>>())
        };
        // A failed run proves nothing about any theorem.
        record(d.path(), "1-check-a", 1, "failed", &["k.Storage.db_inv"], h5i_app_report::inputs(d.path()));
        assert_eq!(checks(&p), (RunStatus::Failed, vec![TheoremCheck::RunNotPassed; 2]));
        // A later passed run on these inputs: the required theorem passed, the
        // catalogued one on standard axioms passed.
        record(d.path(), "2-check-b", 2, "passed", &["k.Storage.db_inv"], h5i_app_report::inputs(d.path()));
        assert_eq!(checks(&p), (RunStatus::Passed, vec![TheoremCheck::Passed; 2]));
        // A run that gated neither by name says so.
        record(d.path(), "3-check-c", 3, "passed", &[], h5i_app_report::inputs(d.path()));
        let r = project(&p);
        assert_eq!(r.theorems[0].check, TheoremCheck::NotGated);
        assert_eq!(r.theorems[1].check, TheoremCheck::Passed);
        // Sources changed since: no longer a pass on the current code.
        std::fs::write(d.path().join("proofs/Storage.lean"), "theorem db_inv : False := sorry\n").unwrap();
        let r = project(&p);
        assert!(!r.check.inputs_current);
        assert_eq!(r.theorems[1].check, TheoremCheck::PassedOnOtherInputs);
    }
}
