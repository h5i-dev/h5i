//! The per-route guarantee report (`h5i app report`): for each project and
//! each HTTP route, what the kernel theorems cover, which assumptions (by
//! ledger ID) the route rests on, and what nothing verifies.
//!
//! Everything in it is derived from the sources, `h5i-app.toml` and the
//! recorded `h5i app check` runs; a theorem is `passed` only if a recorded
//! check run gated it on the current inputs. The report says what is claimed
//! and by what; it proves nothing itself (docs/design/design-app-trust.md,
//! "Per-route guarantee report"). A `passed` run under `.h5i/app/runs` is a
//! local record of a run, not evidence: a repository can ship a forged one,
//! and only running `h5i app check` again establishes the result.

use crate::assumptions::ASSUMPTIONS;
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;

/// Bump on any change a reader of version 1 would misread.
pub const VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Guarantees {
    pub version: u32,
    /// `h5i-app-report <version>`.
    pub generator: String,
    /// The ledger entries the routes cite, from [`ASSUMPTIONS`].
    pub assumptions: Vec<AssumptionEntry>,
    pub projects: Vec<Project>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AssumptionEntry {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Project {
    /// The project directory, relative to the repository root when inside one.
    pub path: String,
    /// The extracted kernel crate, relative to `path`.
    pub kernel: Option<String>,
    /// `[authz] guarded` is set: authorization is framework-proven.
    pub authz: bool,
    /// The most recent recorded `h5i app check` run.
    pub check: CheckRun,
    pub theorems: Vec<Theorem>,
    pub routes: Vec<Route>,
    /// The framework-defined obligations (`H5iAppLib.Obligation`) and what
    /// the latest passed check run found for each; `not_stated` ones say
    /// why no statement exists.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub obligations: Vec<crate::ObligationResult>,
    /// What could not be analyzed; a route or theorem it hides is missing
    /// from the report, so a non-empty list means the report is incomplete.
    pub errors: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CheckRun {
    /// Its id under `.h5i/app/runs`, if there is one.
    pub run: Option<String>,
    pub status: RunStatus,
    /// The run's input digest equals the current sources' (`Inputs`).
    pub inputs_current: bool,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    NotRun,
    Running,
    Passed,
    Failed,
    Interrupted,
    /// A status this version does not know.
    Other,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Theorem {
    /// The Lean name, as `h5i app check` gates it.
    pub name: String,
    /// Where the requirement comes from.
    pub source: TheoremSource,
    /// The Lean file that states it, relative to the project, if found.
    pub file: Option<String>,
    pub covers: Covers,
    /// What the most recent check run observed for it.
    pub check: TheoremCheck,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TheoremSource {
    /// `[check] theorems` in h5i-app.toml.
    CheckTheorems,
    /// Generated for `[authz] guarded` (and its storage theorems, or `[authz] storage`).
    Authz,
    /// A hand-written proof stating `H5iAppLib.ReadOnly.Sound`, for a kernel
    /// that overrides `Kernel::read_only`.
    ReadOnlyProof,
    /// Named in `[obligations]`, or generated, for a framework-defined
    /// obligation: `check` requires exactly the obligation's statement.
    Obligation,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Covers {
    /// A statement over every command (`transition`, `apply`, the store).
    AllCommands,
    /// Only the commands `read_only` declares.
    ReadOnlyCommands,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TheoremCheck {
    /// The last check run passed, gated this theorem (standard axioms only),
    /// and its inputs are the current ones.
    Passed,
    /// As `passed`, but on other inputs: sources changed since.
    PassedOnOtherInputs,
    /// The last check run did not pass (failed, interrupted, or still running).
    RunNotPassed,
    /// The last check run passed without gating this theorem by name.
    NotGated,
    /// No check run is recorded.
    NotRun,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Route {
    /// `GET`, `POST`, ...
    pub method: String,
    pub path: String,
    /// `file:line` of the route's registration, relative to the project.
    pub at: String,
    /// The handler as written (`h5i_app_http::rpc` for `rpc_router`).
    pub handler: String,
    /// The handler takes an `Actor` (an authenticated caller); `None` if the
    /// handler could not be found.
    pub actor: Option<bool>,
    /// The route runs kernel commands through the engine.
    pub kernel: bool,
    /// The kernel commands it may build.
    pub commands: Vec<Command>,
    /// How `commands` was found.
    pub commands_from: CommandsFrom,
    /// Names of the project's theorems that cover this route.
    pub theorems: Vec<String>,
    /// Ledger IDs this route's guarantees rest on.
    pub assumptions: Vec<String>,
    /// What neither a theorem nor a listed assumption covers.
    pub not_verified: Vec<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CommandsFrom {
    /// `rpc_router`: the JSON decoder may produce any `Command`, so every
    /// variant is listed.
    EveryVariant,
    /// `Command::…` paths in the handler's body.
    HandlerBody,
    /// No kernel command.
    None,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Command {
    /// The `Command` variant.
    pub name: String,
    pub path: EnginePath,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EnginePath {
    /// `Kernel::read_only` declares it: a `SERIALIZABLE READ ONLY DEFERRABLE`
    /// transaction without locks (unless sent with an idempotency key).
    ReadOnly,
    /// A read-write SERIALIZABLE transaction.
    Write,
    /// `read_only` is overridden in a way the report cannot read.
    Unknown,
}

/// Not verified for every route that reaches the kernel.
pub const KERNEL_ROUTE_NOT_VERIFIED: &[&str] = &[
    "request decoding: the JSON-to-Command mapping (A2; theorems hold for every command)",
    "reply rendering: the app's Reply-to-Value mapping (A3; the JSON writer itself is proven)",
    "authentication: the HMAC key, its storage and the expiry clock (A1)",
    "axum, hyper and tokio delivering requests and responses intact",
    "PostgreSQL's implementation of SERIALIZABLE and of the rendered SQL (A4, A5)",
    "the engine h5i-app-pg: tested against PostgreSQL with injected faults, not proven (A6)",
    "Charon, Aeneas and Lean (A7)",
];

/// Not verified for a route whose handler takes no `Actor`.
pub const NO_ACTOR: &str = "the principal: the handler takes no Actor, so it chooses whom the command runs as";

/// Not verified for a route that does not reach the kernel.
pub const OUTSIDE_KERNEL: &[&str] = &[
    "the handler: it does not go through the kernel, so no theorem covers it",
    "axum, hyper and tokio delivering requests and responses intact",
];

impl Guarantees {
    pub fn new(projects: Vec<Project>) -> Self {
        let mut cited: Vec<&str> = projects.iter().flat_map(|p| &p.routes).flat_map(|r| &r.assumptions).map(String::as_str).collect();
        cited.sort();
        cited.dedup();
        let assumptions = ASSUMPTIONS
            .iter()
            .filter(|a| cited.contains(&a.id))
            .map(|a| AssumptionEntry { id: a.id.into(), name: a.name.into() })
            .collect();
        Guarantees { version: VERSION, generator: format!("h5i-app-report {}", env!("CARGO_PKG_VERSION")), assumptions, projects }
    }

    /// Parse a report, refusing other versions and unknown fields.
    pub fn parse(text: &str) -> Result<Self, String> {
        let g: Guarantees = serde_json::from_str(text).map_err(|e| e.to_string())?;
        if g.version != VERSION {
            return Err(format!("guarantee report version {} (this reader knows {VERSION})", g.version));
        }
        Ok(g)
    }

    /// The human-readable form.
    pub fn render(&self) -> String {
        let mut s = String::new();
        for p in &self.projects {
            let _ = writeln!(s, "{}{}", p.path, if p.authz { " [authz]" } else { "" });
            let run = match &p.check.run {
                Some(id) => format!("{} ({id}{})", word(&p.check.status), if p.check.inputs_current { "" } else { ", sources changed since" }),
                None => "no recorded run".into(),
            };
            let _ = writeln!(s, "  h5i app check: {run}");
            if p.check.run.is_some() {
                let _ = writeln!(s, "    (a local record of a run, not evidence: rerun `h5i app check` to establish it)");
            }
            if p.theorems.is_empty() {
                let _ = writeln!(s, "  theorems: none required");
            } else {
                let _ = writeln!(s, "  theorems:");
                for t in &p.theorems {
                    let covers = match t.covers {
                        Covers::AllCommands => "every command",
                        Covers::ReadOnlyCommands => "read-only commands",
                    };
                    let _ = writeln!(
                        s,
                        "    {} [{}] {covers}; {}",
                        t.name,
                        word(&t.check),
                        t.file.as_deref().unwrap_or("file not found")
                    );
                }
            }
            if !p.obligations.is_empty() {
                let _ = writeln!(s, "  obligations:");
                for o in &p.obligations {
                    let by = o.theorem.as_deref().map(|t| format!(" by {t}")).unwrap_or_default();
                    let _ = writeln!(s, "    {} [{}]{by}", o.name, o.status);
                }
            }
            for r in &p.routes {
                let actor = match r.actor {
                    Some(true) => "Actor",
                    Some(false) => "no Actor",
                    None => "handler not found",
                };
                let _ = writeln!(s, "  {} {}  ({}; {actor}; {})", r.method, r.path, r.handler, r.at);
                if r.kernel {
                    let cmds: Vec<String> = r
                        .commands
                        .iter()
                        .map(|c| match c.path {
                            EnginePath::ReadOnly => format!("{} (read-only)", c.name),
                            EnginePath::Write => c.name.clone(),
                            EnginePath::Unknown => format!("{} (path unknown)", c.name),
                        })
                        .collect();
                    let _ = writeln!(s, "    commands: {}", if cmds.is_empty() { "none found".into() } else { cmds.join(", ") });
                    let _ = writeln!(s, "    theorems: {}", if r.theorems.is_empty() { "none".into() } else { r.theorems.join(", ") });
                } else {
                    let _ = writeln!(s, "    no kernel command");
                }
                let _ = writeln!(s, "    assumes: {}", if r.assumptions.is_empty() { "-".into() } else { r.assumptions.join(" ") });
                for n in &r.not_verified {
                    let _ = writeln!(s, "    not verified: {n}");
                }
            }
            for e in &p.errors {
                let _ = writeln!(s, "  error: {e}");
            }
        }
        if !self.assumptions.is_empty() {
            let _ = writeln!(s, "assumptions:");
            for a in &self.assumptions {
                let _ = writeln!(s, "  {}: {}", a.id, a.name);
            }
        }
        s
    }
}

/// An enum's serialized name, for the text form.
fn word<T: Serialize>(v: &T) -> String {
    serde_json::to_value(v).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Guarantees {
        Guarantees::new(vec![Project {
            path: "examples/app/x".into(),
            kernel: Some("kernel".into()),
            authz: false,
            check: CheckRun { run: None, status: RunStatus::NotRun, inputs_current: false },
            theorems: vec![Theorem {
                name: "x.db_inv".into(),
                source: TheoremSource::CheckTheorems,
                file: Some("proofs/Storage.lean".into()),
                covers: Covers::AllCommands,
                check: TheoremCheck::NotRun,
            }],
            routes: vec![Route {
                method: "POST".into(),
                path: "/rpc".into(),
                at: "server/src/main.rs:3".into(),
                handler: "h5i_app_http::rpc".into(),
                actor: Some(true),
                kernel: true,
                commands: vec![Command { name: "Get".into(), path: EnginePath::Write }],
                commands_from: CommandsFrom::EveryVariant,
                theorems: vec!["x.db_inv".into()],
                assumptions: vec!["A1".into(), "A6".into()],
                not_verified: vec![],
            }],
            errors: vec![],
            obligations: vec![],
        }])
    }

    #[test]
    fn round_trips_and_refuses_unknown_fields_and_versions() {
        let g = sample();
        assert_eq!(g.assumptions.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), ["A1", "A6"]);
        let text = serde_json::to_string(&g).unwrap();
        assert_eq!(Guarantees::parse(&text).unwrap(), g);
        // A claim the schema does not have is refused, not ignored.
        let extra = text.replacen("\"kernel\":true", "\"kernel\":true,\"proven\":true", 1);
        assert!(Guarantees::parse(&extra).is_err());
        let v2 = text.replacen("\"version\":1", "\"version\":2", 1);
        assert!(Guarantees::parse(&v2).unwrap_err().contains("version 2"));
        assert!(g.render().contains("POST /rpc"));
    }
}
