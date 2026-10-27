//! `h5i app extract`: the kernel crate to Lean, Rust -> LLBC (Charon) -> Lean
//! (Aeneas), into `proofs/generated/`.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use regex::Regex;

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
        // Charon takes an impl pattern only as the first element of a path.
        let path = if s.starts_with("{impl ") { s.clone() } else { format!("{id}::{s}") };
        charon.args(["--start-from", &path]);
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
    // `Schema.lean` first: the storage proof is read off it.
    if ex.schema && schema_lean {
        bless_schema(&krate, out)?;
    }
    authz_lean(&p.manifest.authz.guarded, p.manifest.authz.storage.as_deref(), ex.std_specs, &module, &id, dest)?;
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
/// debug builds (scripts/no-debug-guard.sh) can run it.
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
/// so the proofs go through unchanged. `AuthzSpecs` is copied too when the
/// kernel calls the authorization guard (`h5i_app_std::authz`).
fn std_specs(meta: &util::Metadata, module: &str, ns: &str, dest: &Path) -> Result<()> {
    let std = meta
        .packages
        .iter()
        .find(|p| p.name == "h5i-app-std")
        .context("`std-specs = true` but the crate does not depend on h5i-app-std")?;
    let proofs = std.manifest_path.parent().unwrap().join("proofs");
    copy_spec(&proofs, "StdSpecs.lean", module, ns, dest)?;
    let authz = dest.join("AuthzSpecs.lean");
    let kernel = std::fs::read_to_string(dest.join(format!("{module}.lean")))?;
    if calls_guard(&kernel) {
        copy_spec(&proofs, "AuthzSpecs.lean", module, ns, dest)?;
    } else if authz.is_file() {
        std::fs::remove_file(authz)?;
    }
    Ok(())
}

/// The generated module holding an app's authorization theorems.
pub const AUTHZ_MODULE: &str = "Authz";

/// Write `Authz.lean` for `[authz] guarded`: one `h5i_authz` per guarded
/// kernel function, each instantiating the framework's theorem. Removes a
/// stale one when the list is empty.
fn authz_lean(guarded: &[String], storage: Option<&str>, std_specs: bool, module: &str, ns: &str, dest: &Path) -> Result<()> {
    let file = dest.join(format!("{AUTHZ_MODULE}.lean"));
    if guarded.is_empty() {
        if file.is_file() {
            std::fs::remove_file(&file)?;
        }
        return Ok(());
    }
    if !std_specs {
        bail!("`[authz] guarded` needs `std-specs = true` in [extract]: the theorem comes with h5i-app-std's AuthzSpecs");
    }
    if !dest.join("AuthzSpecs.lean").is_file() {
        bail!(
            "`[authz] guarded` names {}, but {module}.lean never calls `h5i_app_std::authz::guard`; \
             is each one a start point in [extract]?",
            guarded.join(", ")
        );
    }
    let mut text = authz_text(guarded, ns);
    let schema = std::fs::read_to_string(dest.join("Schema.lean")).unwrap_or_default();
    let kernel = std::fs::read_to_string(dest.join(format!("{module}.lean"))).unwrap_or_default();
    let tables = schema_tables(&schema);
    match (storage, sql_arms(&kernel)) {
        // The app proves A4 itself; `check` requires that theorem instead.
        (Some(_), _) => {}
        // The storage proof builds on `Schema.lean`'s store section, which
        // `schema!` writes only with `writes` and a snapshot.
        (None, Some(arms)) if !tables.is_empty() && schema.contains("theorem apply_spec'") && schema.contains("theorem sql_writes_spec'") => {
            text = text.replacen("import AuthzSpecs\n", "import AuthzSpecs\nimport Schema\n", 1);
            text.push_str(&storage_text(ns, &tables, &arms));
        }
        (None, _) => bail!(
            "cannot generate the storage proof for `[authz]`: it needs `schema = true` with `writes`, and a \
             `sql_write` whose arms only push `schema!` table writes. Prove that the store commits what \
             `apply` computes by hand (as examples/app/board's Storage.lean does) and name the theorem as \
             `[authz] storage = \"<theorem>\"`"
        ),
    }
    std::fs::write(&file, text)?;
    Ok(())
}

/// A table of the schema, read from the generated `Schema.lean`.
#[derive(Debug, PartialEq)]
pub struct Table {
    pub name: String,
    pub index: usize,
    /// Has key columns (so `delA`, `delWhereA` and the `map_*` lemmas exist).
    pub keyed: bool,
}

/// `def <T>.table : Nat := <i>` and `def <T>.keyLen : Nat := <k>` lines.
pub fn schema_tables(schema: &str) -> Vec<Table> {
    static TABLE: std::sync::LazyLock<Regex> =
        std::sync::LazyLock::new(|| Regex::new(r"(?m)^def (\w+)\.table : Nat := (\d+)$").unwrap());
    static KEY: std::sync::LazyLock<Regex> =
        std::sync::LazyLock::new(|| Regex::new(r"(?m)^def (\w+)\.keyLen : Nat := (\d+)$").unwrap());
    let mut tables: Vec<Table> = TABLE
        .captures_iter(schema)
        .filter_map(|c| Some(Table { name: c[1].to_string(), index: c[2].parse().ok()?, keyed: false }))
        .collect();
    for c in KEY.captures_iter(schema) {
        if let Some(t) = tables.iter_mut().find(|t| t.name == c[1]) {
            t.keyed = c[2] != *"0";
        }
    }
    tables.sort_by_key(|t| t.index);
    tables
}

/// The table writes of each `Write` constructor, read off the extracted
/// `sql_write`: `(".Ctor binders", ["T.putA x", ...])`. `None` unless every
/// line of every arm is a table write (`let wN ← T.sql_put/sql_del/sql_del_where
/// args`), a push, or `ok out`: the shape `schema!` tables produce. The
/// storage proof checks the result against the extracted code, so a wrong
/// reading fails the build instead of proving something false.
pub fn sql_arms(kernel: &str) -> Option<Vec<(String, Vec<String>)>> {
    use std::sync::LazyLock;
    static BODY: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?s)\ndef sql_write\n.*?\n  match w with\n(.*?)(?:\n/--|\nend |\z)").unwrap());
    static ARM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\| Write\.(\w+(?: \w+)*) =>(?: (.*))?$").unwrap());
    static WRITE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^let \w+ ← (\w+)\.sql_(put|del_where|del) (\S+(?: \S+)*)$").unwrap()
    });
    static PUSH: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^(?:let \w+ ← )?alloc\.vec\.Vec\.push \w+ \w+$|^ok \w+$").unwrap());
    static TWO: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\S+) (\S+)$").unwrap());

    let body = BODY.captures(kernel)?.get(1)?.as_str();
    let mut arms: Vec<(String, Vec<String>)> = Vec::new();
    for line in body.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let rest = match ARM.captures(line) {
            Some(c) => {
                arms.push((format!(".{}", &c[1]), Vec::new()));
                match c.get(2) {
                    Some(r) => r.as_str().trim(),
                    None => continue,
                }
            }
            None => line,
        };
        let arm = arms.last_mut()?;
        if PUSH.is_match(rest) {
            continue;
        }
        let c = WRITE.captures(rest)?;
        let (table, args) = (&c[1], &c[3]);
        arm.1.push(match &c[2] {
            "put" => format!("{table}.putA {args}"),
            "del" => format!("{table}.delA {args}"),
            _ => {
                let cv = TWO.captures(args)?;
                format!("{table}.delWhereA {}.val {}", &cv[1], &cv[2])
            }
        });
    }
    if arms.is_empty() { None } else { Some(arms) }
}

/// The storage section of `Authz.lean`: the store commits what the kernel's
/// `apply` computes (A4), with no hand-written model. The state is the table
/// encoding itself, so `enc_step` holds by construction; what is checked
/// against the extracted code is that `sql_write` emits `sqlA` (`sql_fits`)
/// and that `apply_write` changes `Snapshot.enc` exactly as `sqlA` does
/// (`apply_write_enc`).
pub fn storage_text(ns: &str, tables: &[Table], arms: &[(String, Vec<String>)]) -> String {
    let n = tables.len();
    let list = |f: &dyn Fn(&Table) -> Vec<String>| tables.iter().flat_map(f).collect::<Vec<_>>().join(", ");
    let writes = list(&|t: &Table| {
        let mut v = vec![format!("{}.putA", t.name), format!("{}.table", t.name)];
        if t.keyed {
            v.extend([format!("{}.delA", t.name), format!("{}.delWhereA", t.name)]);
        }
        v
    });
    let step_lemmas = list(&|t: &Table| {
        let mut v = vec![format!("{}.keyLen", t.name)];
        if t.keyed {
            v.extend([format!("← {}.map_put", t.name), format!("← {}.map_del", t.name), format!("← {}.map_del_where", t.name)]);
        } else {
            v.push(format!("{}.row", t.name));
        }
        v
    });
    let arms_text: String = arms
        .iter()
        .map(|(pat, ws)| format!("  | {pat} => [{}]\n", ws.join(", ")))
        .collect();
    format!(
        r#"
/-! ## The store commits what `apply` computes

A4 for this app, with no hand-written model: the state is the table encoding
itself (`trunc` keeps the schema's {n} tables), so `enc_step` holds by
construction. What is checked against the extracted code is that `sql_write`
emits `sqlA` (`sql_fits`) and that `apply_write` changes `Snapshot.enc`
exactly as `sqlA` does (`apply_write_enc`). Then `apply_enc`: committing a
write set leaves the encoding of the kernel's `apply`; `pg_stored`: on
PostgreSQL, the tenant's tables hold that state and every load decodes to it,
up to row order. `sqlA` is read off the extracted `sql_write`; a wrong reading
fails here. -/

set_option linter.unusedSimpArgs false

namespace {ns}.Committed
open {ns}.Schema H5iAppLib.Sql H5iAppLib.Store

def sqlA : Write → List (AWrite Val)
{arms_text}
def trunc (T : Tables Val) : Tables Val := fun t => if t < {n} then T t else []

theorem targets (w : Write) : ∀ a ∈ sqlA w, target a < {n} := by
  cases w <;> simp [sqlA, target, {writes}]

theorem enc_nil (s : Snapshot) (t : Nat) (h : {n} ≤ t) : Snapshot.enc s t = [] := by
  obtain ⟨k, rfl⟩ := Nat.exists_eq_add_of_le' h
  simp only [Snapshot.enc]

theorem trunc_enc (s : Snapshot) : trunc (Snapshot.enc s) = Snapshot.enc s := by
  funext t; by_cases ht : t < {n}
  · simp [trunc, ht]
  · simp [trunc, ht, enc_nil s t (by omega)]

theorem enc_step (T : Tables Val) (w : Write) :
    applyAllW kl (trunc T) (sqlA w) = trunc (applyAllW kl T (sqlA w)) := by
  funext t
  by_cases ht : t < {n}
  · simp only [trunc, ht, if_true]
    exact applyAllW_at kl _ _ _ t (by simp [trunc, ht])
  · rw [applyAllW_other kl _ t (fun a ha => by have := targets w a ha; omega)]
    simp [trunc, ht]

noncomputable def app : App (Tables Val) Write Val where
  kl := kl
  enc := trunc
  sql := sqlA
  step T w := applyAllW kl T (sqlA w)
  init := Snapshot.enc Snapshot.empty
  IsRow := IsRow
  enc_step := enc_step
  sql_ok := by schema_ok [sqlA]
  init_ok := by rw [trunc_enc]; schema_init [Snapshot.enc, Snapshot.empty]
  init_rows := by rw [trunc_enc]; schema_rows [Snapshot.enc, Snapshot.empty]

theorem fits : Fits app Snapshot.enc where
  kl := rfl
  rows := rfl
  enc s := trunc_enc s
  init := trunc_enc _
  nil s t h := by simp [app, trunc]; omega

/-- `sql_write` emits `sqlA`. -/
theorem sql_fits : SqlFits app := by
  intro w out h
  unfold sql_write; cases w <;> simp only [app, sqlA, List.length_singleton, List.length_nil] at h ⊢ <;> step* <;> simp_all

/-- `apply_write` changes the encoding exactly as `sqlA` does. -/
theorem apply_write_enc (s : Snapshot) (w : Write) (h : Snapshot.size s < Usize.max) :
    apply_write s w ⦃ s' => Snapshot.enc s' = app.step (Snapshot.enc s) w ∧
      Snapshot.size s' ≤ Snapshot.size s + 1 ⦄ := by
  unfold Snapshot.size at *
  unfold apply_write
  cases w <;> step*
  all_goals (try refine ⟨?_, ?_⟩)
  all_goals
    first
    | (funext t; cases_table t <;> simp_all [app, sqlA, Snapshot.enc, applyAllW, applyW, kl, upsert, {writes}, {step_lemmas}] <;>
        (congr 1; funext r; congr 1; exact decide_eq_decide.2 Iff.rfl))
    | (simp_all [alloc.vec.Vec.length] <;> grind [upsert_length, List.length_filter_le])

/-- Committing a write set leaves the encoding of the kernel's `apply`. -/
theorem apply_enc (s : Snapshot) (ws : alloc.vec.Vec Write) (h : Snapshot.size s + ws.length < Usize.max) :
    apply s ws ⦃ s' => Snapshot.enc s' = ws.val.foldl app.step (Snapshot.enc s) ⦄ :=
  apply_spec' Snapshot.enc app.step apply_write_enc (fun w => by simp) s ws h

/-- On PostgreSQL, for any accepted write sets (`Step`) interleaved with other
tenants' statements: the tenant's tables hold the state, and every load
decodes to it up to row order. -/
theorem pg_stored {{ts : List Pg.Tab}} {{tv : Val}} {{Step : Snapshot → alloc.vec.Vec Write → Prop}}
    (hv : Pg.Valid ts) (htv : valKind tv = some .int) (hkl : Pg.klOf ts = kl) (hlen : ts.length = {n})
    {{db : Pg.PgDb Val}} {{s : Tables Val}} (h : PgServed app Snapshot.enc ts tv Step db s) :
    app.Holds (Pg.view ts tv db) (app.enc s) ∧
      ∀ r, Loads ts tv db r → decode r ⦃ o => ∃ snap, o = some snap ∧ app.Equiv (Snapshot.enc snap) s ⦄ := by
  obtain ⟨hm, hS⟩ := pg_served sql_fits fits hv htv hkl hlen h
  refine ⟨(stored fits sql_fits hS).1, fun r hr => ?_⟩
  have hl : Lists kl (Pg.view ts tv db) (Rows.tabs r) := by
    rw [← hkl]
    exact Pg.lists_of_selects valKind hv htv hm _ (fun t ht => Rows.tabs_nil _ t (hlen ▸ ht))
      (fun t ht => hr t (hlen ▸ ht))
  exact (stored fits sql_fits hS).2 r hl

end {ns}.Committed
"#
    )
}

/// The storage theorems `h5i app check` requires when `Authz.lean` has them.
pub const STORAGE_THEOREMS: [&str; 3] = ["Committed.sql_fits", "Committed.apply_enc", "Committed.pg_stored"];

pub fn authz_text(guarded: &[String], ns: &str) -> String {
    let mut s = format!(
        "-- Written by `h5i app extract` from `[authz] guarded` in h5i-app.toml. Do not edit.\n\
         import AuthzSpecs\n\
         /-!\n\
         # Framework-proven authorization\n\n\
         Each `h5i_authz g` reads the policy's instances off `g`'s extracted body and\n\
         proves `g.authorized` by instantiating `h5i_app_std.Specs.authz`: every\n\
         committed command makes only writes the policy permits and discloses only rows\n\
         it lets the caller see. For `guard_viewed` it also proves noninterference of\n\
         read-only commands and `H5iAppLib.ReadOnly.Sound` (`g.read_only_sound`).\n\
         -/\n\
         open Aeneas Aeneas.Std Result H5iAppLib {ns}\n\n\
         h5i_derive_all\n\n"
    );
    for g in guarded {
        s.push_str(&format!("h5i_authz {ns}.{g}\n"));
    }
    s
}

/// The kernel's extraction holds `h5i_app_std::authz::guard` or `guard_viewed`.
fn calls_guard(kernel: &str) -> bool {
    kernel.contains("h5i_app_std.authz.guard")
}

/// Copy one spec file of h5i-app-std, rewriting its header to import the
/// kernel's module and open its namespace.
fn copy_spec(proofs: &Path, name: &str, module: &str, ns: &str, dest: &Path) -> Result<()> {
    let src = proofs.join(name);
    let text = std::fs::read_to_string(&src).with_context(|| {
        format!(
            "reading {} (this h5i-app-std ships without its proofs)",
            src.display()
        )
    })?;
    let (mut import, mut open) = (false, false);
    let mut out = format!("-- Copied from h5i-app-std's proofs/{name} by `h5i app extract`. Do not edit.\n");
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
    std::fs::write(dest.join(name), out)?;
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
    fn storage_reads_tables_and_sql_write() {
        let schema = "def User.table : Nat := 0\ndef User.keyLen : Nat := 1\ndef Counter.table : Nat := 1\ndef Counter.keyLen : Nat := 0\n";
        assert_eq!(
            schema_tables(schema),
            vec![
                Table { name: "User".into(), index: 0, keyed: true },
                Table { name: "Counter".into(), index: 1, keyed: false }
            ]
        );
        let kernel = "x\ndef sql_write\n  (w : Write) (out : V) :\n  R\n  := do\n  match w with\n  | Write.PutUser u => let w1 ← User.sql_put u\n                       alloc.vec.Vec.push out w1\n  | Write.SetCounter c =>\n    let w1 ← Counter.sql_put c\n    alloc.vec.Vec.push out w1\n  | Write.DelUsers c v => let w1 ← User.sql_del_where c v\n    alloc.vec.Vec.push out w1\n  | Write.Emit _ => ok out\n\n/-- next -/\n";
        assert_eq!(
            sql_arms(kernel).unwrap(),
            vec![
                (".PutUser u".to_string(), vec!["User.putA u".to_string()]),
                (".SetCounter c".to_string(), vec!["Counter.putA c".to_string()]),
                (".DelUsers c v".to_string(), vec!["User.delWhereA c.val v".to_string()]),
                (".Emit _".to_string(), vec![]),
            ]
        );
        let odd = kernel.replace("let w1 ← Counter.sql_put c", "let w1 ← mk c");
        assert!(sql_arms(&odd).is_none(), "an arm that is not a table write: no storage proof");
    }

    #[test]
    fn authz_lean_has_one_line_per_kernel() {
        let t = authz_text(&["guarded_transition".into(), "admin".into()], "forum_kernel");
        assert!(t.contains("import AuthzSpecs\n"));
        assert!(t.contains("open Aeneas Aeneas.Std Result H5iAppLib forum_kernel\n"));
        assert!(t.contains("h5i_authz forum_kernel.guarded_transition\nh5i_authz forum_kernel.admin\n"));
    }

    #[test]
    fn authz_specs_follow_the_guard() {
        assert!(calls_guard("def guarded_transition := h5i_app_std.authz.guard inst"));
        assert!(calls_guard("h5i_app_std.authz.guard_viewed a b"));
        assert!(!calls_guard("h5i_app_std.set.contains inst s x"));
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
