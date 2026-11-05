//! Framework-defined obligations: the statements h5i fixes, instantiated for
//! one app (`H5iAppLib.Obligation` in `crates/h5i-app-core/proofs`).
//!
//! Each obligation is a Lean `def … : Prop` in the library, applied here to
//! the app's extracted functions and schema: the kernel's `transition` (or
//! `guarded_transition`), `read_only`, `apply`, `sql_writes`, `decode`, and the
//! schema's `Snapshot.enc`, `Loads` and `kl`. Nothing in the statement comes
//! from the app's proofs, so a theorem cannot discharge a weaker one: `check`
//! elaborates the statement in its gate program and requires the theorem's
//! type to be definitionally equal to it. A `_` marks a part the app chooses
//! (the write list of a write set that is not a `Vec`, a hand-written policy);
//! the gate prints what it was resolved to, for review.
//!
//! Who discharges each one:
//!
//! - a theorem the manifest names (`[obligations]`, `[authz] storage`);
//! - a theorem the framework generates (`h5i_authz`'s, the storage section's);
//! - `check`'s automation (`Auto.lean`, tried in a separate Lean run whose
//!   failures are reported as `open`, never as `sorry`);
//! - nobody: `open`.
//!
//! The intent-free ones (group A of `docs/design/design-app-authz.md`,
//! "Obligations") are stated for every project with an extracted kernel:
//! panic freedom, tenant isolation, read-only soundness when commands are
//! read-only, and storage for `[authz]`.

use std::path::Path;

use anyhow::Result;

use crate::manifest::Project;

/// One obligation, instantiated for a project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Obligation {
    /// `read-only`, `storage`, `panic-free`, `authorized`, `tenant-isolation`,
    /// `authz`: the `[obligations]` key, or the generated slot's name.
    pub name: &'static str,
    /// The statement, a Lean term of type `Prop`; `_` holes are the app's choice.
    pub statement: String,
    /// The theorem that must have exactly this statement, if one is named or
    /// generated. `check` fails if it is missing or states something else.
    pub theorem: Option<String>,
    /// A proof `check` tries when no theorem is named; on failure the
    /// obligation is reported `open`.
    pub auto: Option<String>,
    /// Why there is no statement or proof, or what stays trusted.
    pub note: Option<String>,
}

/// What the extraction says about the kernel, read off its Lean.
#[derive(Debug, Default)]
pub struct Kernel {
    /// Lean namespace (the crate identifier).
    pub ns: String,
    /// `def <name>` of the extracted module, top-level only.
    defs: Vec<String>,
    /// `transition`'s write set is an `alloc.vec.Vec`.
    vec_writes: bool,
    /// Tables of the generated `Schema.lean`, if any.
    tables: usize,
    /// `Schema.lean` has the store section (`writes` and a snapshot).
    store: bool,
    /// The most table writes one `Write` makes (`sql_write`'s arms), if read.
    max_arm: Option<usize>,
}

impl Kernel {
    pub fn read(p: &Project) -> Result<Option<Kernel>> {
        let Some(krate) = p.krate() else { return Ok(None) };
        let ns = match crate::util::package_name(&krate) {
            Some(n) => crate::util::ident(&n),
            None => crate::check::kernel_ns(p)?,
        };
        Ok(Some(Kernel::from_lean(&ns, &p.generated())))
    }

    pub fn from_lean(ns: &str, generated: &Path) -> Kernel {
        let module = crate::util::lean_module(ns);
        let text = std::fs::read_to_string(generated.join(format!("{module}.lean"))).unwrap_or_default();
        let schema = std::fs::read_to_string(generated.join("Schema.lean")).unwrap_or_default();
        Kernel::from_text(ns, &text, &schema)
    }

    pub fn from_text(ns: &str, text: &str, schema: &str) -> Kernel {
        let defs: Vec<String> = text
            .lines()
            .filter_map(|l| l.strip_prefix("def "))
            .filter_map(|r| r.split_whitespace().next())
            .map(str::to_string)
            .collect();
        // `def transition\n  (actor : …) … :\n  Result (core.result.Result ((alloc.vec.Vec W) × R) E)`.
        let vec_writes = text
            .split("def transition\n")
            .nth(1)
            .and_then(|rest| rest.split(":=").next())
            .is_some_and(|sig| sig.contains("core.result.Result ((alloc.vec.Vec "));
        let max_arm = crate::extract::sql_arms(text).map(|arms| arms.iter().map(|(_, ws)| ws.len()).max().unwrap_or(0));
        Kernel {
            ns: ns.to_string(),
            defs,
            vec_writes,
            tables: crate::extract::schema_tables(schema).len(),
            store: schema.contains("theorem apply_spec'") && schema.contains("theorem sql_writes_spec'"),
            max_arm,
        }
    }

    fn has(&self, f: &str) -> bool {
        self.defs.iter().any(|d| d == f)
    }

    fn writes_of(&self, guarded: bool) -> &'static str {
        if guarded || self.vec_writes { "(fun ws => ws.val)" } else { "_" }
    }
}

/// The generated storage theorem, when `extract` writes it (see [`storage_generated`]).
pub const STORAGE_THEOREM: &str = "Committed.storage";

/// `extract` proves the storage obligation (`Committed.storage`) when it
/// writes the storage section and each `Write` makes at most one table
/// write: then the bound on the snapshot also bounds the table writes.
pub fn storage_generated(k: &Kernel) -> bool {
    k.store && k.tables > 0 && k.max_arm.is_some_and(|m| m <= 1)
}

/// The storage statement for kernel `k`.
pub fn storage_statement(k: &Kernel) -> String {
    let ns = &k.ns;
    format!(
        "H5iAppLib.Obligation.Storage {ns}.Schema.valKind (fun ts tv db r => {ns}.Schema.Loads ts tv db r) {ns}.decode \
         {ns}.sql_writes {ns}.Schema.sqlW {ns}.apply {ns}.Schema.Snapshot.empty {ns}.Schema.Snapshot.enc \
         (fun snap ws => {ns}.Schema.Snapshot.size snap + ws.length < Aeneas.Std.Usize.max) {ns}.Schema.kl {}",
        k.tables
    )
}

/// Every obligation of `p`, in report order.
pub fn of(p: &Project) -> Result<Vec<Obligation>> {
    let Some(k) = Kernel::read(p)? else { return Ok(Vec::new()) };
    Ok(of_kernel(p, &k))
}

pub fn of_kernel(p: &Project, k: &Kernel) -> Vec<Obligation> {
    let m = &p.manifest;
    let ns = &k.ns;
    let authz = !m.authz.guarded.is_empty();
    // The function the engine runs: `Authz<T>`'s guard, or the kernel's own.
    let subject = if authz { format!("{ns}.guarded_transition") } else { format!("{ns}.transition") };
    let has_subject = k.has(if authz { "guarded_transition" } else { "transition" });
    let mut out = Vec::new();

    if authz {
        out.push(Obligation {
            name: "authz",
            statement: format!(
                "h5i_app_std.Specs.authz.AuthorizedViewed {subject} _ _ _ _ ∧ h5i_app_std.Specs.authz.ViewNoninterference {subject} _"
            ),
            theorem: Some(format!("{subject}.authorized")),
            auto: None,
            note: Some("the guard's theorem for the instances `h5i_authz` reads off `guarded_transition`".into()),
        });
    }

    // Read-only soundness: for `[authz]` the guard's, else the app's if it names one.
    let read_only = if authz { Some(format!("{subject}.read_only_sound")) } else { m.obligations.read_only.clone() };
    if read_only.is_some() || k.has("read_only") {
        out.push(Obligation {
            name: "read-only",
            statement: format!(
                "H5iAppLib.Obligation.ReadOnlySound {ns}.read_only {subject} {}",
                k.writes_of(authz)
            ),
            theorem: read_only,
            auto: None,
            note: (!authz && !k.vec_writes).then(|| {
                "the write set is not a `Vec`: the theorem's write list (`writesOf`) is the app's, printed by `check`".into()
            }),
        });
    }

    if authz {
        let (theorem, note) = match p.storage_theorem() {
            Some(t) => (Some(t.to_string()), None),
            None if storage_generated(k) => (Some(format!("{ns}.{STORAGE_THEOREM}")), None),
            None => (
                None,
                Some(
                    "`extract` proves it only when every `Write` makes at most one table write; otherwise name a theorem \
                     as `[obligations] storage`"
                        .into(),
                ),
            ),
        };
        if k.store {
            out.push(Obligation { name: "storage", statement: storage_statement(k), theorem, auto: None, note });
        }
    } else if let Some(t) = &m.obligations.storage {
        out.push(Obligation { name: "storage", statement: storage_statement(k), theorem: Some(t.clone()), auto: None, note: None });
    }

    if let Some(t) = &m.obligations.authorized {
        out.push(Obligation {
            name: "authorized",
            statement: format!("H5iAppLib.Obligation.Authorized {ns}.transition _ {} _ _ _", k.writes_of(false)),
            theorem: Some(t.clone()),
            auto: None,
            note: Some("`allowed` and `visible` are the app's: `check` prints them, and they are reviewed (A10)".into()),
        });
    }

    if has_subject {
        let theorem = m.obligations.panic_free.clone();
        let auto = theorem.is_none().then(|| format!("by\n  intro a s c\n  unfold {subject}\n  step*"));
        out.push(Obligation {
            name: "panic-free",
            statement: format!("H5iAppLib.Obligation.PanicFree {subject}"),
            theorem,
            auto,
            note: None,
        });
    }

    if k.tables > 0 {
        out.push(Obligation {
            name: "tenant-isolation",
            statement: format!("H5iAppLib.Obligation.TenantIsolation {ns}.Schema.valKind"),
            theorem: None,
            auto: Some(format!("H5iAppLib.Obligation.tenant_isolation {ns}.Schema.valKind")),
            note: Some(
                "on the database model; trusted: the engine and the generated store pass `K::tenant(actor)` consistently \
                 (A6), PostgreSQL behaves as `H5iAppLib.Pg` (A4), `schema_spec()` is valid (checked at runtime)"
                    .into(),
            ),
        });
    }
    out
}

/// Obligations that are not stated, and why: shown by the report so their
/// absence is not mistaken for a proof.
pub fn not_stated(p: &Project, k: &Kernel) -> Vec<(&'static str, String)> {
    if !k.has("transition") && !k.has("guarded_transition") {
        return Vec::new();
    }
    let mut v = vec![(
        "reply-codec",
        "`ReplyCodec` (fingerprint, scope, encode/decode) is implemented in the server crate and not extracted; stating \
         `decode (encode r) = ok r`, fingerprint injectivity and a `/`-free scope needs the codec moved into the kernel \
         crate in the Aeneas subset (bytes as `Vec<u8>`, no `String` or serde)"
            .to_string(),
    )];
    if !p.manifest.authz.guarded.is_empty() && !k.store {
        v.push(("storage", "no store section in Schema.lean (`schema = true` with `writes` and a snapshot)".into()));
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    const KERNEL: &str = "def transition\n  (actor : Principal) (s : Snapshot) (cmd : Command) :\n  Result (core.result.Result ((alloc.vec.Vec Write) × Reply) Error)\n  := do\n  ok x\n\ndef read_only (cmd : Command) : Result Bool := do\n  ok true\n";

    fn project(toml: &str) -> Project {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("h5i-app.toml"), toml).unwrap();
        let p = Project::load(&d.path().join("h5i-app.toml")).unwrap();
        std::mem::forget(d);
        p
    }

    #[test]
    fn statements_name_the_extracted_functions() {
        let k = Kernel::from_text("k", KERNEL, "def User.table : Nat := 0\ndef User.keyLen : Nat := 1\n");
        let p = project("[obligations]\nread-only = \"k.ro\"\n");
        let o = of_kernel(&p, &k);
        let ro = o.iter().find(|o| o.name == "read-only").unwrap();
        assert_eq!(ro.statement, "H5iAppLib.Obligation.ReadOnlySound k.read_only k.transition (fun ws => ws.val)");
        assert_eq!(ro.theorem.as_deref(), Some("k.ro"));
        let pf = o.iter().find(|o| o.name == "panic-free").unwrap();
        assert!(pf.theorem.is_none() && pf.auto.is_some());
        assert!(o.iter().any(|o| o.name == "tenant-isolation"));
        assert!(o.iter().all(|o| o.name != "storage"), "no [authz]: no storage slot unless named");
    }

    #[test]
    fn a_write_set_that_is_not_a_vec_leaves_the_write_list_to_the_app() {
        let k = Kernel::from_text("c", &KERNEL.replace("(alloc.vec.Vec Write)", "(Option Memory)"), "");
        let p = project("[obligations]\nread-only = \"c.ro\"\n");
        let ro = of_kernel(&p, &k).into_iter().find(|o| o.name == "read-only").unwrap();
        assert!(ro.statement.ends_with(" _"), "{}", ro.statement);
        assert!(ro.note.is_some());
    }

    #[test]
    fn authz_uses_the_guarded_function() {
        let text = format!("{KERNEL}\ndef guarded_transition\n  (a : P) : R := do\n  ok x\n");
        let k = Kernel { store: true, tables: 3, max_arm: Some(1), ..Kernel::from_text("f", &text, "") };
        let p = project("[authz]\nguarded = [\"guarded_transition\"]\n");
        let o = of_kernel(&p, &k);
        let ro = o.iter().find(|o| o.name == "read-only").unwrap();
        assert_eq!(ro.theorem.as_deref(), Some("f.guarded_transition.read_only_sound"));
        assert!(ro.statement.contains("f.read_only f.guarded_transition"));
        let st = o.iter().find(|o| o.name == "storage").unwrap();
        assert_eq!(st.theorem.as_deref(), Some("f.Committed.storage"));
        assert!(st.statement.ends_with("f.Schema.kl 3"), "{}", st.statement);
        let k2 = Kernel { max_arm: Some(2), ..k };
        let st = of_kernel(&p, &k2).into_iter().find(|o| o.name == "storage").unwrap();
        assert!(st.theorem.is_none() && st.note.is_some(), "two table writes per write: open");
    }
}
