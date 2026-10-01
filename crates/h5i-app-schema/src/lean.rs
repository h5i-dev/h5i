//! Renders a schema's Lean file: row encodings and a lemma for every
//! function `schema!` generates, in the form `H5iAppLib.Store` needs.
//!
//! App column types (enums) need `T.col`, `T.col_to_val`, `T.col_from_val`
//! and a simp lemma `T.col_inj` in a hand-written `Columns.lean`.

use std::fmt::Write;

/// One row type: name, table, fields as (name, Rust type), key length.
pub type RowDecl<'a> = (&'a str, &'a str, Vec<(&'a str, &'a str)>, usize);

/// The snapshot: its name and fields as (field, row type).
pub type SnapDecl<'a> = (&'a str, Vec<(&'a str, &'a str)>);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Prim {
    U64,
    U32,
    Bool,
    Bytes,
    OptU64,
}

enum Col<'a> {
    Prim(Prim),
    Own(&'a str),
}

fn col(ty: &str) -> Col<'_> {
    match ty {
        "u64" => Col::Prim(Prim::U64),
        "u32" => Col::Prim(Prim::U32),
        "bool" => Col::Prim(Prim::Bool),
        "alloc::vec::Vec<u8>" => Col::Prim(Prim::Bytes),
        "core::option::Option<u64>" => Col::Prim(Prim::OptU64),
        other => Col::Own(other.rsplit("::").next().unwrap_or(other)),
    }
}

/// Lean term for the SQL value of field term `x`.
fn expr(c: &Col, x: &str) -> String {
    match c {
        Col::Prim(Prim::U64 | Prim::U32) => format!("int {x}.val"),
        Col::Prim(Prim::Bool) => format!(".Bool {x}"),
        Col::Prim(Prim::Bytes) => format!(".Bytes {x}"),
        Col::Prim(Prim::OptU64) => format!("optV {x}"),
        Col::Own(t) => format!("{t}.col {x}"),
    }
}

fn lean_ty(c: &Col) -> String {
    match c {
        Col::Prim(Prim::U64) => "U64".into(),
        Col::Prim(Prim::U32) => "U32".into(),
        Col::Prim(Prim::Bool) => "Bool".into(),
        Col::Prim(Prim::Bytes) => "alloc.vec.Vec U8".into(),
        Col::Prim(Prim::OptU64) => "Option U64".into(),
        Col::Own(t) => t.to_string(),
    }
}

/// A fresh tenant's value (`h5i_app_sql::Zero`).
fn zero(c: &Col) -> String {
    match c {
        Col::Prim(Prim::U64) => "0#u64".into(),
        Col::Prim(Prim::U32) => "0#u32".into(),
        Col::Prim(Prim::Bool) => "false".into(),
        Col::Prim(Prim::Bytes) => "alloc.vec.Vec.new U8".into(),
        Col::Prim(Prim::OptU64) => "none".into(),
        Col::Own(t) => panic!("schema!: a singleton row cannot have a column of type {t}"),
    }
}

fn module(krate: &str) -> String {
    krate.split('_').map(|w| {
        let mut c = w.chars();
        c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
    }).collect()
}

const U64: &str = r#"
@[simp] theorem u64_val_eq (x y : U64) : x.val = y.val ↔ x = y :=
  ⟨fun h => by scalar_tac, fun h => h ▸ rfl⟩

theorem int_inj {a b : Nat} (ha : a < 2 ^ 64) (hb : b < 2 ^ 64) (h : int a = int b) : a = b := by
  simp only [int, h5i_app_sql.Val.Int.injEq, IScalar.mk.injEq] at h
  have := congrArg BitVec.toNat h
  simp only [BitVec.toNat_ofNat] at this
  rwa [Nat.mod_eq_of_lt (by simpa using ha), Nat.mod_eq_of_lt (by simpa using hb)] at this

@[simp] theorem int_u64 (x y : U64) : int x.val = int y.val ↔ x = y := by
  refine ⟨fun h => ?_, fun h => h ▸ rfl⟩
  have := int_inj (by scalar_tac) (by scalar_tac) h
  scalar_tac

theorem u64_to_val_eq (x : U64) : U64.Insts.H5i_app_sqlColumn.to_val x = ok (int x.val) := by
  unfold U64.Insts.H5i_app_sqlColumn.to_val int
  simp only [lift, UScalar.hcast]
  rw [← UScalar.bv_toNat, BitVec.ofNat_toNat]
  simp [BitVec.zeroExtend]

@[step] theorem u64_to_val (x : U64) : U64.Insts.H5i_app_sqlColumn.to_val x ⦃ v => v = int x.val ⦄ := by
  simp [u64_to_val_eq]

theorem u64_from_val (x : U64) : U64.Insts.H5i_app_sqlColumn.from_val (int x.val) = ok (some x) := by
  simp only [U64.Insts.H5i_app_sqlColumn.from_val, int, lift, IScalar.hcast]
  rw [← UScalar.bv_toNat, BitVec.ofNat_toNat]
  simp [BitVec.signExtend_eq]
"#;

const BOOL: &str = r#"
@[step] theorem bool_to_val (b : Bool) : Bool.Insts.H5i_app_sqlColumn.to_val b ⦃ v => v = .Bool b ⦄ := by
  simp [Bool.Insts.H5i_app_sqlColumn.to_val]

theorem bool_from_val (b : Bool) : Bool.Insts.H5i_app_sqlColumn.from_val (.Bool b) = ok (some b) := by
  simp [Bool.Insts.H5i_app_sqlColumn.from_val]
"#;

const BYTES: &str = r#"
theorem u8vec_clone (v : alloc.vec.Vec U8) : alloc.vec.CloneVec.clone core.clone.CloneU8 v = ok v :=
  vec_clone_eq _ v (fun _ => rfl)

@[step] theorem bytes_to_val (v : alloc.vec.Vec U8) :
    alloc.vec.VecU8.Insts.H5i_app_sqlColumn.to_val v ⦃ x => x = .Bytes v ⦄ := by
  simp [alloc.vec.VecU8.Insts.H5i_app_sqlColumn.to_val, u8vec_clone]

theorem bytes_from_val (v : alloc.vec.Vec U8) :
    alloc.vec.VecU8.Insts.H5i_app_sqlColumn.from_val (.Bytes v) = ok (some v) := by
  simp [alloc.vec.VecU8.Insts.H5i_app_sqlColumn.from_val, u8vec_clone]
"#;

const OPT: &str = r#"
def optV : Option U64 → Val
  | some x => int x.val
  | none => .Null

@[step] theorem opt_to_val (o : Option U64) :
    core.option.Option.Insts.H5i_app_sqlColumn.to_val U64.Insts.H5i_app_sqlColumn o ⦃ x => x = optV o ⦄ := by
  cases o with
  | none => simp [core.option.Option.Insts.H5i_app_sqlColumn.to_val, optV]
  | some x => simpa [core.option.Option.Insts.H5i_app_sqlColumn.to_val, optV] using u64_to_val x

theorem opt_from_val (o : Option U64) :
    core.option.Option.Insts.H5i_app_sqlColumn.from_val U64.Insts.H5i_app_sqlColumn (optV o) = ok (some o) := by
  cases o with
  | none => simp [core.option.Option.Insts.H5i_app_sqlColumn.from_val, optV]
  | some x =>
    simp only [core.option.Option.Insts.H5i_app_sqlColumn.from_val, optV, int]
    rw [← int, u64_from_val]
    simp

@[simp] theorem optV_inj (a b : Option U64) : optV a = optV b ↔ a = b := by
  cases a <;> cases b <;> simp only [optV, reduceCtorEq, Option.some.injEq, int_u64] <;> simp [int]
"#;

const COMPARE: &str = r#"
/-! ## Comparing encoded columns -/

@[step] theorem val_eq_spec (a b : Val) : h5i_app_sql.val_eq a b ⦃ r => (r = true ↔ a = b) ⦄ := by
  cases a <;> cases b <;> simp only [h5i_app_sql.val_eq] <;> first
    | exact WP.spec_mono (vec_u8_eq_spec _ _) (fun r h => by simp [h])
    | simp

@[step] theorem has_col_spec (row : alloc.vec.Vec Val) (col : U32) (val : Val) :
    h5i_app_sql.has_col row col val ⦃ r => (r = true ↔ row.val[col.val]? = some val) ⦄ := by
  unfold h5i_app_sql.has_col
  step*
  · rename_i h; simp_all [List.getElem?_eq_getElem (show col.val < row.val.length by scalar_tac)]
  · rename_i h; simp only [Bool.false_eq_true, false_iff]
    rw [List.getElem?_eq_none (by scalar_tac)]; simp
"#;

const SQLW: &str = r#"
/-- A table write as a list value. -/
def sqlW : h5i_app_sql.Write → AWrite Val
  | .Put t n row => .put t.val n.val row.val
  | .Del t k => .del t.val k.val
  | .DelWhere t c v => .delWhere t.val c.val v
"#;

/// The generated Lean file for a kernel crate `krate`.
pub fn render(krate: &str, rows: &[RowDecl], snap: Option<&SnapDecl>, writes: Option<&str>) -> String {
    let mut used: Vec<Prim> = Vec::new();
    let mut own: Vec<&str> = Vec::new();
    let mut zeros: Vec<Prim> = Vec::new();
    for (_, _, fields, key_len) in rows {
        for (_, ty) in fields {
            match col(ty) {
                Col::Prim(p) => {
                    if !used.contains(&p) {
                        used.push(p);
                    }
                    if *key_len == 0 && !zeros.contains(&p) {
                        zeros.push(p);
                    }
                }
                Col::Own(t) => {
                    if !own.contains(&t) {
                        own.push(t);
                    }
                }
            }
        }
    }
    let has = |p: Prim| used.contains(&p);
    let ints = has(Prim::U64) || has(Prim::U32) || has(Prim::OptU64);
    let keyed = rows.iter().any(|(.., k)| *k > 0);
    let names: Vec<&str> = rows.iter().map(|(n, ..)| *n).collect();
    let singles: Vec<&str> = rows.iter().filter(|(.., k)| *k == 0).map(|(n, ..)| *n).collect();
    let n = rows.len();
    let tcases = {
        let mut p = String::new();
        for _ in 0..n {
            p.push_str("_ | ");
        }
        p.push('t');
        p
    };

    let mut s = String::new();
    let m = module(krate);
    let _ = writeln!(s, "-- Generated by h5i_app_schema::schema! from `{krate}`. Do not edit; H5I_APP_BLESS=1 rewrites it.");
    let _ = writeln!(s, "import {m}");
    let _ = writeln!(s, "import H5iAppLib");
    if !own.is_empty() {
        let _ = writeln!(s, "import Columns");
    }
    let _ = writeln!(
        s,
        "/-!\n# Rows of `{krate}` as SQL values\n\nKey columns first; tables numbered in declaration order.\n-/"
    );
    let _ = writeln!(s, "open Aeneas Aeneas.Std Result {krate} H5iAppLib H5iAppLib.Sql H5iAppLib.Store\n");
    let _ = writeln!(s, "namespace {krate}.Schema\n");
    let _ = writeln!(s, "open Classical\n");
    let _ = writeln!(s, "abbrev Val := h5i_app_sql.Val\n");
    let _ = writeln!(
        s,
        "/-- How the PostgreSQL driver types a value; `none` is `NULL`. -/
def valKind : Val → Option Pg.Kind
  | .Int _ => some .int
  | .Bool _ => some .bool
  | .Text _ => some .text
  | .Bytes _ => some .bytes
  | .Null => none\n"
    );
    if ints {
        let _ = writeln!(s, "/-- An integer column: its 64 bits as a `BIGINT`. -/\ndef int (n : Nat) : Val := .Int ⟨BitVec.ofNat _ n⟩");
        s.push_str(U64);
    }
    if has(Prim::Bool) {
        s.push_str(BOOL);
    }
    if has(Prim::Bytes) {
        s.push_str(BYTES);
    }
    if has(Prim::OptU64) {
        s.push_str(OPT);
    }
    for t in &own {
        let _ = writeln!(
            s,
            "\n@[step] theorem {t}_col_to_val (x : {t}) : {t}.Insts.H5i_app_sqlColumn.to_val x ⦃ v => v = {t}.col x ⦄ := by\n  simp [{t}.col_to_val]"
        );
    }
    for p in &zeros {
        let (inst, ty, z) = match p {
            Prim::U64 => ("U64.Insts.H5i_app_sqlZero.zero", "U64", "0#u64"),
            Prim::U32 => ("U32.Insts.H5i_app_sqlZero.zero", "U32", "0#u32"),
            Prim::Bool => ("Bool.Insts.H5i_app_sqlZero.zero", "Bool", "false"),
            Prim::Bytes => ("alloc.vec.VecU8.Insts.H5i_app_sqlZero.zero", "alloc.vec.Vec U8", "alloc.vec.Vec.new U8"),
            Prim::OptU64 => ("(core.option.Option.Insts.H5i_app_sqlZero.zero U64)", "Option U64", "none"),
        };
        let lname = match p {
            Prim::U64 => "u64",
            Prim::U32 => "u32",
            Prim::Bool => "bool",
            Prim::Bytes => "bytes",
            Prim::OptU64 => "opt",
        };
        let _ = writeln!(
            s,
            "\n@[step] theorem {lname}_zero : {inst} ⦃ x => x = ({z} : {ty}) ⦄ := by\n  simp [{}]",
            inst.trim_start_matches('(').split(' ').next().unwrap_or(inst)
        );
    }
    if keyed {
        s.push_str(COMPARE);
    }
    s.push_str(SQLW);

    // Lemmas `row_tac` reads columns with.
    let mut dec = vec!["List.getElem_cons_zero", "List.getElem_cons_succ", "Nat.zero_add", "Nat.reduceAdd", "bind_tc_ok"];
    if ints {
        dec.push("u64_from_val");
    }
    if has(Prim::Bool) {
        dec.push("bool_from_val");
    }
    if has(Prim::Bytes) {
        dec.push("bytes_from_val");
    }
    if has(Prim::OptU64) {
        dec.push("opt_from_val");
    }
    let own_from: Vec<String> = own.iter().map(|t| format!("{t}.col_from_val")).collect();
    dec.extend(own_from.iter().map(|x| x.as_str()));

    for (i, (name, table, fields, key_len)) in rows.iter().enumerate() {
        let names_f: Vec<String> = fields.iter().map(|(f, _)| format!("`{f}`")).collect();
        let cols: Vec<String> = fields.iter().map(|(f, ty)| expr(&col(ty), &format!("x.{f}"))).collect();
        let nf = fields.len();
        let mut clone_extra = String::new();
        if fields.iter().any(|(_, ty)| matches!(col(ty), Col::Prim(Prim::Bytes))) {
            clone_extra.push_str("u8vec_clone, ");
        }
        let own_clones: String = fields.iter().filter_map(|(_, ty)| match col(ty) {
            Col::Own(t) => Some(format!("\n  try (simp [{t}.Insts.CoreCloneClone.clone])")),
            _ => None,
        }).collect();

        let _ = writeln!(s, "\n/-! ## `{name}` (table `{table}`) -/\n");
        let _ = writeln!(s, "/-- Columns {}. -/\ndef {name}.row (x : {name}) : List Val :=\n  [{}]\n", names_f.join(", "), cols.join(", "));
        let _ = writeln!(s, "def {name}.table : Nat := {i}\ndef {name}.keyLen : Nat := {key_len}");
        for (j, (field, _)) in fields.iter().enumerate() {
            let _ = writeln!(s, "def {name}.col_{field} : Nat := {j}");
        }
        s.push('\n');
        for (field, ty) in fields {
            let value = expr(&col(ty), &format!("x.{field}"));
            let _ = writeln!(
                s,
                "@[simp] theorem {name}.row_col_{field} (x : {name}) :\n    ({name}.row x)[{name}.col_{field}]? = some ({value}) := by\n  simp [{name}.row, {name}.col_{field}]\n"
            );
        }
        let _ = writeln!(s, "@[simp] theorem {name}.row_length (x : {name}) : ({name}.row x).length = {nf} := rfl\n");
        let _ = writeln!(
            s,
            "@[step] theorem {name}.to_row_spec (x : {name}) : {name}.to_row x ⦃ v => v.val = {name}.row x ⦄ := by\n  have := usize_max_le\n  unfold {name}.to_row; step* <;> simp_all [{name}.row] <;> scalar_tac\n"
        );
        let _ = writeln!(
            s,
            "theorem {name}.row_inj : Function.Injective {name}.row := by\n  intro a b h; cases a; cases b; simp_all [{name}.row]\n"
        );
        let _ = writeln!(
            s,
            "theorem {name}.clone_eq (x : {name}) : {name}.Insts.CoreCloneClone.clone x = ok x := by\n  simp [{name}.Insts.CoreCloneClone.clone, {clone_extra}lift]{own_clones}\n"
        );
        let _ = writeln!(
            s,
            "@[step] theorem {name}.clone_spec (x : {name}) : {name}.Insts.CoreCloneClone.clone x ⦃ y => y = x ⦄ := by\n  rw [{name}.clone_eq]; simp"
        );
        let _ = writeln!(s, "\ndef {name}.putA (x : {name}) : AWrite Val := .put {name}.table {name}.keyLen ({name}.row x)\n");
        let _ = writeln!(
            s,
            "@[step] theorem {name}.sql_put_spec (x : {name}) : {name}.sql_put x ⦃ w => sqlW w = {name}.putA x ⦄ := by\n  unfold {name}.sql_put; step*; simp_all [sqlW, {name}.putA, {name}.table, {name}.keyLen, {name}.TABLE, {name}.KEY_LEN]"
        );
        if *key_len == 0 {
            let zs: Vec<String> = fields.iter().map(|(f, ty)| format!("{f} := {}", zero(&col(ty)))).collect();
            let _ = writeln!(s, "\n/-- A fresh tenant's `{name}`: all zeros. -/\ndef {name}.zero : {name} := {{ {} }}", zs.join(", "));
            continue;
        }
        let keys = &fields[..*key_len];
        let key_of = |v: &str| -> String {
            if keys.len() == 1 {
                format!("{v}.{}", keys[0].0)
            } else {
                let ks: Vec<String> = keys.iter().map(|(k, _)| format!("{v}.{k}")).collect();
                format!("({})", ks.join(", "))
            }
        };
        let key_is = |v: &str| -> String {
            if keys.len() == 1 {
                format!("({v}.{} = {})", keys[0].0, keys[0].0)
            } else {
                let ks: Vec<String> = keys.iter().map(|(k, _)| format!("{v}.{k} = {k}")).collect();
                format!("({})", ks.join(" ∧ "))
            }
        };
        let params: Vec<String> = keys.iter().map(|(k, ty)| format!("({k} : {})", lean_ty(&col(ty)))).collect();
        let args: Vec<&str> = keys.iter().map(|(k, _)| *k).collect();
        let key_vals: Vec<String> = keys.iter().map(|(k, ty)| expr(&col(ty), k)).collect();
        let kf = format!("fun y => {}", key_of("y"));
        let _ = writeln!(
            s,
            "
/-- Insert, or replace the row with the same key. -/
@[step] theorem {name}.put_spec (v : alloc.vec.Vec {name}) (x : {name}) (h : v.length < Usize.max) :
    {name}.put v x ⦃ v' => v'.val = upsert ({kf}) x v.val ⦄ := by
  unfold {name}.put {name}.put_loop
  apply WP.spec_mono (loop_search v.val (fun q => decide ({} = {}))
    (fun y : alloc.vec.Vec {name} => y.val) (fun j _ => v.val.set j x) (v.val ++ [x]) _ ?_ 0#usize (by simp))
  · intro r hr; rw [hr, show ((0#usize : Usize) : Nat) = 0 from rfl]
    exact upsert_loop_result ({kf}) x _ 0 (Nat.zero_le _) (by simp)
  · intro j hj; unfold {name}.put_loop.body; h5i_step",
            key_of("q"),
            key_of("x")
        );
        let _ = writeln!(
            s,
            "
/-- The rows without this key. -/
@[step] theorem {name}.del_spec (v : alloc.vec.Vec {name}) {} :
    {name}.del v {} ⦃ v' => v'.val = v.val.filter (fun y => ¬{}) ⦄ := by
  unfold {name}.del {name}.del_loop
  apply WP.spec_mono (loop_fold v.val (fun w : alloc.vec.Vec {name} => w.val)
    (fun acc y => if decide ¬{} then acc ++ [y] else acc)
    (fun w j => w.length ≤ j) (fun x => {name}.del_loop.body v {} x.1 x.2) ?_ _ 0#usize (by simp) (by simp))
  · intro r hr; rw [hr, foldl_filter]; simp
  · intro o j hj ho; have := v.len_ineq; unfold {name}.del_loop.body; h5i_step",
            params.join(" "),
            args.join(" "),
            key_is("y"),
            key_is("y"),
            args.join(" ")
        );
        let _ = writeln!(
            s,
            "
/-- The rows whose column `col` does not hold `val`. -/
@[step] theorem {name}.del_where_spec (v : alloc.vec.Vec {name}) (col : U32) (val : Val) :
    {name}.del_where v col val ⦃ v' => v'.val = v.val.filter (fun y => ¬(({name}.row y)[col.val]? = some val)) ⦄ := by
  unfold {name}.del_where {name}.del_where_loop
  apply WP.spec_mono (loop_fold v.val (fun w : alloc.vec.Vec {name} => w.val)
    (fun acc y => if decide ¬(({name}.row y)[col.val]? = some val) then acc ++ [y] else acc)
    (fun w j => w.length ≤ j) (fun x => {name}.del_where_loop.body v col val x.1 x.2) ?_ _ 0#usize (by simp) (by simp))
  · intro r hr; rw [hr, foldl_filter]; simp
  · intro o j hj ho; have := v.len_ineq; unfold {name}.del_where_loop.body; h5i_step"
        );
        let _ = writeln!(
            s,
            "
def {name}.delA {} : AWrite Val := .del {name}.table [{}]
def {name}.delWhereA (col : Nat) (val : Val) : AWrite Val := .delWhere {name}.table col val

@[step] theorem {name}.sql_del_spec {} : {name}.sql_del {} ⦃ w => sqlW w = {name}.delA {} ⦄ := by
  unfold {name}.sql_del; step* <;> simp_all [sqlW, {name}.delA, {name}.table, {name}.TABLE] <;> scalar_tac

@[step] theorem {name}.sql_del_where_spec (col : U32) (val : Val) :
    {name}.sql_del_where col val ⦃ w => sqlW w = {name}.delWhereA col.val val ⦄ := by
  unfold {name}.sql_del_where; simp [sqlW, {name}.delWhereA, {name}.table, {name}.TABLE]",
            params.join(" "),
            key_vals.join(", "),
            params.join(" "),
            args.join(" "),
            args.join(" ")
        );
        let _ = writeln!(
            s,
            "
/-! Row writes on the encoding match the list operations. -/

@[simp] theorem {name}.map_put (x : {name}) (l : List {name}) :
    upsert (·.take {key_len}) ({name}.row x) (l.map {name}.row) = (upsert ({kf}) x l).map {name}.row :=
  map_upsert _ _ _ _ _ (fun y => by simp [{name}.row])

@[simp] theorem {name}.map_del {} (l : List {name}) :
    (l.map {name}.row).filter (fun r => !decide (r.take {key_len} = [{}])) =
      (l.filter (fun y => !decide {})).map {name}.row :=
  map_filter_of _ _ _ _ (fun y => by simp [{name}.row])

@[simp] theorem {name}.map_del_where (col : Nat) (val : Val) (l : List {name}) :
    (l.map {name}.row).filter (fun r => !decide (r[col]? = some val)) =
      (l.filter (fun y => !decide (({name}.row y)[col]? = some val))).map {name}.row := by
  rw [List.filter_map]; rfl",
            params.join(" "),
            key_vals.join(", "),
            key_is("y")
        );
    }

    let row_defs: Vec<String> = names.iter().map(|n| format!("{n}.row")).collect();
    let _ = writeln!(
        s,
        "\n/-! ## Decoding -/\n\n/-- Steps through a generated `from_row`. -/\nmacro \"row_tac\" : tactic => `(tactic| (\n  repeat (first\n    | (step*; done)\n    | (simp_all only [{}, {}]; step*))\n  all_goals simp_all))",
        row_defs.join(", "),
        dec.join(", ")
    );
    for (name, _, fields, _) in rows {
        let nf = fields.len();
        let _ = writeln!(
            s,
            "\ntheorem {name}.from_row_spec (x : {name}) (v : alloc.vec.Vec Val) (h : v.val = {name}.row x) :\n    {name}.from_row v ⦃ o => o = some x ⦄ := by\n  have hl : v.length = {nf} := by simp [alloc.vec.Vec.length, h, {name}.row]\n  unfold {name}.from_row; row_tac"
        );
    }
    for (name, _, _, key_len) in rows {
        if *key_len == 0 {
            let _ = writeln!(
                s,
                "
theorem {name}.from_one_spec (rows : alloc.vec.Vec (alloc.vec.Vec Val)) (l : List {name})
    (hl : l.length ≤ 1) (h : rows.val.map (·.val) = l.map {name}.row) :
    {name}.from_one rows ⦃ o => o = some (l.headD {name}.zero) ⦄ := by
  unfold {name}.from_one
  match l, hl with
  | [], _ =>
    have hr : rows.val = [] := by simpa using h
    have h0 : rows.len = 0#usize := by scalar_tac
    rw [if_pos h0]; step*; simp_all [{name}.zero]
  | [x], _ =>
    obtain ⟨v, hv, hvv⟩ : ∃ v, rows.val = [v] ∧ v.val = {name}.row x := by
      simp only [List.map_cons, List.map_nil] at h
      obtain ⟨v, rest⟩ := List.map_eq_singleton_iff.1 h
      exact ⟨v, rest.1, rest.2⟩
    have h0 : ¬ rows.len = 0#usize := by scalar_tac
    have h1 : rows.len = 1#usize := by scalar_tac
    rw [if_neg h0, if_pos h1]
    step as ⟨ w, hw ⟩
    have : w.val = {name}.row x := by rw [hw]; simp [hv, hvv]
    step with {name}.from_row_spec x w this
    simp_all"
            );
        } else {
            let _ = writeln!(
                s,
                "
theorem {name}.from_rows_spec (rows : alloc.vec.Vec (alloc.vec.Vec Val)) (l : List {name})
    (h : rows.val.map (·.val) = l.map {name}.row) :
    {name}.from_rows rows ⦃ o => ∃ v, o = some v ∧ v.val = l ⦄ := by
  have e : {name}.from_rows_loop rows (alloc.vec.Vec.new {name}) true 0#usize =
      loop (fun x => rowsBody {name}.from_row rows x.1 x.2.1 x.2.2) (alloc.vec.Vec.new {name}, true, 0#usize) := by
    unfold {name}.from_rows_loop; congr 1; funext ⟨a, b, c⟩
    simp only []; unfold {name}.from_rows_loop.body rowsBody
    dsimp only
    split <;> (try rfl)
    congr 1; funext v; congr 1; funext o; cases o <;> rfl
  have hl : {name}.from_rows_loop rows (alloc.vec.Vec.new {name}) true 0#usize ⦃ r => r.2 = true ∧ r.1.val = l ⦄ := by
    rw [e]; exact rows_loop {name}.from_row {name}.row (fun x v hv => {name}.from_row_spec x v hv) rows l h
  unfold {name}.from_rows
  step with hl
  simp [ok1_post, l_post]"
            );
        }
    }

    let _ = writeln!(s, "\n/-! ## Tables -/\n\n/-- Key length per table. -/\ndef kl : Nat → Nat");
    for (i, (_, _, _, key_len)) in rows.iter().enumerate() {
        let _ = writeln!(s, "  | {i} => {key_len}");
    }
    let _ = writeln!(s, "  | _ => 0\n");
    for (i, (_, _, _, key_len)) in rows.iter().enumerate() {
        let _ = writeln!(s, "@[simp] theorem kl_{i} : kl {i} = {key_len} := rfl");
    }
    let _ = writeln!(s, "\n/-- `row` encodes a value of table `t`'s row type. -/\ndef IsRow : Nat → List Val → Prop");
    for (i, name) in names.iter().enumerate() {
        let _ = writeln!(s, "  | {i} => fun r => ∃ x : {name}, r = {name}.row x");
    }
    let _ = writeln!(s, "  | _ => fun _ => False");

    let table_of = |t: &str| names.iter().position(|n| *n == t).unwrap_or_else(|| panic!("schema!: no row type {t}"));
    let writes_simp = |single_rows: bool| {
        let mut l: Vec<String> = Vec::new();
        for (name, _, _, key_len) in rows {
            l.push(format!("{name}.putA"));
            l.push(format!("{name}.table"));
            l.push(format!("{name}.keyLen"));
            if *key_len == 0 && single_rows {
                l.push(format!("{name}.row"));
            }
            if *key_len > 0 {
                l.push(format!("{name}.delA"));
                l.push(format!("{name}.delWhereA"));
            }
        }
        l.join(", ")
    };
    let simp_rows = writes_simp(true);
    let simp_ok = writes_simp(false);
    let rows_only: Vec<String> = names.iter().map(|n| format!("{n}.row")).collect();
    let single_witness: Vec<String> = singles.iter().map(|n| format!("exact ⟨{n}.zero, rfl⟩")).collect();

    // Tactics for an app's `Storage.lean`.
    let _ = writeln!(
        s,
        "
/-- Split a goal about table `t` into one goal per table. -/
macro \"cases_table \" t:ident : tactic => `(tactic| rcases $t:ident with {})

/-- A write's table writes map a state's encoding to the next: `schema_step [enc, sqlA, applyWrite]`. -/
macro \"schema_step\" \" [\" ls:Lean.Parser.Tactic.simpLemma,* \"]\" : tactic => `(tactic| (
  intro s w; funext t
  cases w <;> cases_table t <;>
    simp [applyAllW, applyW, upsert, {simp_rows}, $ls,*] <;>
    first
      | exact map_filter_of _ _ _ _ (fun y => by simp [{}])
      | (congr 1; exact List.filter_congr (fun y _ => by simp [{}]))))

/-- Every table write is well formed: `schema_ok [sqlA]`. -/
macro \"schema_ok\" \" [\" ls:Lean.Parser.Tactic.simpLemma,* \"]\" : tactic => `(tactic| (
  intro w; cases w <;>
    simp [WriteOk, RowOk, IsRow, {simp_ok}, $ls,*]))

/-- A fresh tenant's tables: `schema_init [enc, init]`. -/
macro \"schema_init\" \" [\" ls:Lean.Parser.Tactic.simpLemma,* \"]\" : tactic => `(tactic| (
  intro t; cases_table t <;> simp [InitOk, $ls,*]))

/-- A fresh tenant's rows are rows of their tables: `schema_rows [enc, init]`. -/
macro \"schema_rows\" \" [\" ls:Lean.Parser.Tactic.simpLemma,* \"]\" : tactic => `(tactic| (
  intro t r h; cases_table t <;> simp [$ls,*] at h <;> subst h <;> first {}))",
        tcases.replace("| t", "| $t:ident"),
        rows_only.join(", "),
        rows_only.join(", "),
        if single_witness.is_empty() { "| rfl".to_string() } else { single_witness.iter().map(|w| format!("| {w}")).collect::<Vec<_>>().join(" ") }
    );

    if let Some((sname, sfields)) = snap {
        let field_of = |i: usize| -> (&str, &str, bool) {
            let (f, t) = sfields.iter().find(|(_, t)| table_of(t) == i).unwrap_or_else(|| panic!("schema!: the snapshot has no field for {}", names[i]));
            (f, t, rows[i].3 == 0)
        };
        let _ = writeln!(s, "\n/-! ## The snapshot -/\n\n/-- A snapshot's rows, table by table. -/\ndef {sname}.enc (s : {sname}) : Tables Val");
        for i in 0..n {
            let (f, t, one) = field_of(i);
            if one {
                let _ = writeln!(s, "  | {i} => [{t}.row s.{f}]");
            } else {
                let _ = writeln!(s, "  | {i} => s.{f}.val.map {t}.row");
            }
        }
        let _ = writeln!(s, "  | _ => []\n");
        let empty: Vec<String> = sfields
            .iter()
            .map(|(f, t)| if rows[table_of(t)].3 == 0 { format!("{f} := {t}.zero") } else { format!("{f} := alloc.vec.Vec.new {t}") })
            .collect();
        let _ = writeln!(s, "/-- A fresh tenant's snapshot. -/\ndef {sname}.empty : {sname} :=\n  {{ {} }}\n", empty.join(", "));
        let _ = writeln!(s, "/-- Stored rows, table by table. -/\ndef Rows.tabs (r : Rows) : Tables Val");
        for i in 0..n {
            let (f, _, _) = field_of(i);
            let _ = writeln!(s, "  | {i} => r.{f}.val.map (·.val)");
        }
        let _ = writeln!(s, "  | _ => []\n");
        let clones: Vec<String> = sfields
            .iter()
            .map(|(f, t)| {
                if rows[table_of(t)].3 == 0 {
                    format!("{t}.clone_eq")
                } else {
                    format!("vec_clone_eq {t}.Insts.CoreCloneClone s.{f} {t}.clone_eq")
                }
            })
            .collect();
        let _ = writeln!(
            s,
            "theorem {sname}.clone_eq (s : {sname}) : {sname}.Insts.CoreCloneClone.clone s = ok s := by\n  simp [{sname}.Insts.CoreCloneClone.clone,\n    {}]\n",
            clones.join(",\n    ")
        );

        let ones: Vec<usize> = (0..n).filter(|i| rows[*i].3 == 0).collect();
        let mut hyps = String::new();
        for i in &ones {
            let _ = write!(hyps, "\n    (hone{i} : (E {i}).length = 1)");
        }
        let mut prep = String::new();
        let mut steps = String::new();
        let mut cases = String::new();
        // decode reads the snapshot's fields in declaration order.
        for (f, t) in sfields {
            let i = table_of(t);
            if rows[i].3 == 0 {
                let _ = writeln!(
                    prep,
                    "  obtain ⟨l{i}, n{i}, e{i}, f{i}⟩ := one_row {t}.row {t}.zero (Rows.tabs r {i}) (E {i}) hone{i} (hrow {i}) (hR {i})"
                );
                let _ = writeln!(steps, "  step with {t}.from_one_spec r.{f} l{i} n{i} e{i} as ⟨ o{i}, h{i} ⟩\n  subst h{i}; dsimp only");
                let _ = writeln!(cases, "  | {i} => simp only [{sname}.enc, f{i}]; exact .refl _");
            } else {
                let _ = writeln!(
                    prep,
                    "  have h{i} : (Rows.tabs r {i}).Perm (E {i}) := keyed_perm (hR {i})\n  obtain ⟨l{i}, e{i}⟩ := exists_map {t}.row (Rows.tabs r {i}) (fun row h => hrow {i} row (h{i}.subset h))"
                );
                let _ = writeln!(
                    steps,
                    "  step with {t}.from_rows_spec r.{f} l{i} e{i} as ⟨ o{i}, v{i}, ho{i}, hv{i} ⟩\n  subst ho{i}; dsimp only"
                );
                let _ = writeln!(cases, "  | {i} => simp only [{sname}.enc, hv{i}]; rw [← e{i}]; exact h{i}");
            }
        }
        let mut sorted_cases: Vec<&str> = cases.lines().collect();
        sorted_cases.sort_by_key(|l| l.split_whitespace().nth(1).and_then(|x| x.parse::<usize>().ok()).unwrap_or(0));
        let _ = writeln!(
            s,
            "/-- `decode` reads back tables `E` up to row order (an empty singleton table reads as its fresh row). -/
theorem decode_spec (r : Rows) (E : Tables Val)
    (hrow : ∀ t, ∀ row ∈ E t, IsRow t row){hyps}
    (hnil : ∀ t, {n} ≤ t → E t = [])
    (hR : ∀ t, (Rows.tabs r t).Perm (E t) ∨ (Rows.tabs r t = [] ∧ E t = {sname}.enc {sname}.empty t)) :
    decode r ⦃ o => ∃ s, o = some s ∧ ∀ t, ({sname}.enc s t).Perm (E t) ⦄ := by
{prep}  unfold decode
{steps}  refine ⟨_, rfl, fun t => ?_⟩
  match t with
{}
  | t + {n} => simp only [{sname}.enc, hnil (t + {n}) (by omega)]; exact .refl _",
            sorted_cases.join("\n")
        );

        let sizes: Vec<String> = sfields
            .iter()
            .filter(|(_, t)| rows[table_of(t)].3 > 0)
            .map(|(f, _)| format!("s.{f}.length"))
            .collect();
        let _ = writeln!(
            s,
            "\n/-- Rows in a snapshot's keyed tables; a write adds at most one. -/\ndef {sname}.size (s : {sname}) : Nat := {}",
            if sizes.is_empty() { "0".to_string() } else { sizes.join(" + ") }
        );

        let hone_args: Vec<String> = ones.iter().map(|i| format!("(hone {i} rfl (by rw [hf.init]; rfl))")).collect();
        let _ = writeln!(
            s,
            "
/-! ## Loading

An app's `Storage.lean` gives an `H5iAppLib.Store.App` numbered like this schema. -/

section Load
variable {{St W : Type}} (A : App St W Val) (toSt : {sname} → St)

/-- The app's storage matches this schema. -/
structure Fits : Prop where
  kl : A.kl = kl
  rows : A.IsRow = IsRow
  enc : ∀ s, A.enc (toSt s) = {sname}.enc s
  init : A.enc A.init = {sname}.enc {sname}.empty
  nil : ∀ s t, {n} ≤ t → A.enc s t = []

variable {{A toSt}}

/-- Loading a served database decodes to the state it holds, up to row order. -/
theorem loaded (hf : Fits A toSt) {{db : Db Val}} {{s : St}} (h : A.Served db s) (r : Rows)
    (hl : Lists kl db (Rows.tabs r)) :
    decode r ⦃ o => ∃ snap, o = some snap ∧ A.Equiv (toSt snap) s ⦄ := by
  obtain ⟨-, hrow, hone⟩ := A.served_holds h
  have hR := A.served_lists h _ (by rw [hf.kl]; exact hl)
  rw [hf.rows] at hrow
  rw [hf.kl] at hone
  apply WP.spec_mono (decode_spec r (A.enc s) hrow {} (hf.nil s)
    (fun t => by rw [← hf.init]; exact hR t))
  rintro o ⟨snap, rfl, hp⟩
  exact ⟨snap, rfl, fun t => by rw [hf.enc]; exact hp t⟩

end Load",
            hone_args.join(" ")
        );

        if let Some(w) = writes {
            let _ = writeln!(
                s,
                "
/-! ## Committing and storing a write set -/

/-- `apply` folds `apply_write` (as `f` on `abs`), if no table overflows. -/
theorem apply_spec' {{β : Type}} (abs : {sname} → β) (f : β → {w} → β)
    (hw : ∀ s w, {sname}.size s < Usize.max → apply_write s w ⦃ s' => abs s' = f (abs s) w ∧ {sname}.size s' ≤ {sname}.size s + 1 ⦄)
    (hc : ∀ w, {w}.Insts.CoreCloneClone.clone w = ok w)
    (s : {sname}) (ws : alloc.vec.Vec {w}) (h : {sname}.size s + ws.length < Usize.max) :
    apply s ws ⦃ s' => abs s' = ws.val.foldl f (abs s) ⦄ := by
  unfold apply apply_loop
  rw [{sname}.clone_eq]
  simp only [bind_ok]
  apply WP.spec_mono (loop_fold ws.val abs f (fun t k => {sname}.size t + (ws.length - k) < Usize.max)
    (fun x => apply_loop.body ws x.1 x.2) ?_ s 0#usize (by simp) (by simpa using h))
  · intro r hr; rw [hr]; simp
  · intro t j hj ht
    unfold apply_loop.body
    simp only [alloc.vec.Vec.length] at hj ht
    dsimp only
    split
    · rename_i hlt
      step as ⟨ w, hw1 ⟩
      rw [hc]
      simp only [bind_ok]
      step with hw t w (by scalar_tac) as ⟨ t', ht1, ht2 ⟩
      step*
      simp only [FoldStep]
      have := i2_post
      have : j.val < ws.val.length := by scalar_tac
      refine ⟨by scalar_tac, by rw [ht1, hw1], by scalar_tac, by simp only [alloc.vec.Vec.length] at *; omega⟩
    · simp only [WP.spec_ok, FoldStep, and_true]
      scalar_tac

/-- `sql_writes` concatenates `sql_write` over the write set. -/
theorem sql_writes_spec' (f : {w} → List (AWrite Val))
    (hw : ∀ w out, out.length + (f w).length ≤ Usize.max →
      sql_write w out ⦃ out' => out'.val.map sqlW = out.val.map sqlW ++ f w ⦄)
    (ws : alloc.vec.Vec {w}) (h : (ws.val.flatMap f).length ≤ Usize.max) :
    sql_writes ws ⦃ v => v.val.map sqlW = ws.val.flatMap f ⦄ := by
  unfold sql_writes sql_writes_loop
  apply WP.spec_mono (loop_fold ws.val (fun v : alloc.vec.Vec h5i_app_sql.Write => v.val.map sqlW)
    (fun acc w => acc ++ f w) (fun o k => o.length + ((ws.val.drop k).flatMap f).length ≤ Usize.max)
    (fun x => sql_writes_loop.body ws x.1 x.2) ?_ _ 0#usize (by simp) (by simpa using h))
  · intro r hr; rw [hr, foldl_append_flatMap]; simp
  · intro o j hj ho
    unfold sql_writes_loop.body
    dsimp only
    split
    · rename_i hlt
      have hj' : j.val < ws.val.length := by scalar_tac
      step as ⟨ w, hw1 ⟩
      have hd : ws.val.drop j.val = ws.val[j.val] :: ws.val.drop (j.val + 1) := List.drop_eq_getElem_cons hj'
      rw [hd, List.flatMap_cons, List.length_append] at ho
      step with hw w o (by rw [hw1]; omega) as ⟨ o', ho1 ⟩
      step*
      simp only [FoldStep]
      have hl : o'.length = o.length + (f w).length := by
        have := congrArg List.length ho1; simp at this; simpa [alloc.vec.Vec.length] using this
      refine ⟨hj', by rw [ho1, hw1], by scalar_tac, ?_⟩
      rw [i2_post, hl, hw1]; omega
    · simp only [WP.spec_ok, FoldStep, and_true]
      scalar_tac

section Store
variable {{St : Type}} (A : App St {w} Val) (toSt : {sname} → St)

/-- Databases the server reaches from an empty tenant, with the state each holds. -/
inductive Served : Db Val → St → Prop
  | fresh : Served (fun _ _ => none) A.init
  | commit {{db : Db Val}} {{s : St}} {{r : Rows}} {{snap : {sname}}} {{ws : alloc.vec.Vec {w}}}
      {{v : alloc.vec.Vec h5i_app_sql.Write}} :
      Served db s → Lists kl db (Rows.tabs r) → decode r = ok (some snap) →
      (ws.val.flatMap A.sql).length ≤ Usize.max → sql_writes ws = ok v →
      Served (execAll db ((v.val.map sqlW).map planA)) (ws.val.foldl A.step (toSt snap))

variable {{A toSt}}

/-- `sql_write` computes the app's table writes. -/
def SqlFits (A : App St {w} Val) : Prop :=
  ∀ w out, out.length + (A.sql w).length ≤ Usize.max →
    sql_write w out ⦃ out' => out'.val.map sqlW = out.val.map sqlW ++ A.sql w ⦄

theorem served_app (hf : Fits A toSt) (hs : SqlFits A) {{db : Db Val}} {{s : St}} (h : Served A toSt db s) :
    A.Served db s := by
  induction h with
  | fresh => exact .fresh
  | commit _ hl hd hn hv ih =>
    obtain ⟨snap', he, hq⟩ := post_of_ok (loaded hf ih _ hl) hd
    cases he
    rw [post_of_ok (sql_writes_spec' A.sql hs _ hn) hv]
    exact .commit ih hq

/-- Every served database holds its state's rows and loads back to that state. -/
theorem stored (hf : Fits A toSt) (hs : SqlFits A) {{db : Db Val}} {{s : St}} (h : Served A toSt db s) :
    A.Holds db (A.enc s) ∧
      ∀ r, Lists kl db (Rows.tabs r) → decode r ⦃ o => ∃ snap, o = some snap ∧ A.Equiv (toSt snap) s ⦄ :=
  ⟨(A.served_holds (served_app hf hs h)).1, loaded hf (served_app hf hs h)⟩

end Store

/-! ## On PostgreSQL

`PgServed` is one tenant's history on a shared database; other tenants'
statements may run in between. SQL comes from the extracted `h5i_app_pgsql`
(`Pg.selectA`, `Pg.compileA`); the database model is `H5iAppLib.Pg`. `ts` is
`schema_spec()`, `tv` the tenant id. -/

section Pg
variable {{St : Type}} (A : App St {w} Val) (toSt : {sname} → St) (ts : List Pg.Tab) (tv : Val)
  (Step : {sname} → alloc.vec.Vec {w} → Prop)

/-- Rows the compiled `SELECT` of each table returns, in any order. -/
def Loads (db : Pg.PgDb Val) (r : Rows) : Prop :=
  ∀ t, t < {n} → ∃ q ps R0, Pg.selectA valKind ts tv t none = some (q, ps) ∧
    Pg.selected valKind db q ps = some R0 ∧ (Rows.tabs r t).Perm R0

inductive PgServed : Pg.PgDb Val → St → Prop
  | fresh {{db : Pg.PgDb Val}} :
      Pg.Matches valKind ts db → Pg.view ts tv db = (fun _ _ => none) → PgServed db A.init
  | commit {{db db' : Pg.PgDb Val}} {{s : St}} {{r : Rows}} {{snap : {sname}}} {{ws : alloc.vec.Vec {w}}}
      {{v : alloc.vec.Vec h5i_app_sql.Write}} {{qs : List (Pg.Sql × List Val)}} :
      PgServed db s → Loads ts tv db r → decode r = ok (some snap) → Step snap ws →
      (ws.val.flatMap A.sql).length ≤ Usize.max → sql_writes ws = ok v →
      ((v.val.map sqlW).map planA).mapM (Pg.compileA valKind ts tv) = some qs →
      Pg.runAll valKind db qs = some db' → PgServed db' (ws.val.foldl A.step (toSt snap))
  | other {{db db' : Pg.PgDb Val}} {{s : St}} {{tv' : Val}} {{ss : List (AStmt Val)}}
      {{qs : List (Pg.Sql × List Val)}} :
      PgServed db s → tv' ≠ tv → valKind tv' = some .int →
      ss.mapM (Pg.compileA valKind ts tv') = some qs → Pg.runAll valKind db qs = some db' → PgServed db' s

variable {{A toSt ts tv Step}}

theorem Rows.tabs_nil (r : Rows) (t : Nat) (h : {n} ≤ t) : Rows.tabs r t = [] := by
  obtain ⟨k, rfl⟩ := Nat.exists_eq_add_of_le' h
  simp only [Rows.tabs]

/-- The tenant's view of a `PgServed` database is `Served`, with the same state. -/
theorem pg_served (hs : SqlFits A) (hf : Fits A toSt) (hv : Pg.Valid ts) (htv : valKind tv = some .int)
    (hkl : Pg.klOf ts = kl) (hlen : ts.length = {n}) {{db : Pg.PgDb Val}} {{s : St}}
    (h : PgServed A toSt ts tv Step db s) : Pg.Matches valKind ts db ∧ Served A toSt (Pg.view ts tv db) s := by
  induction h with
  | fresh hm he => exact ⟨hm, he ▸ .fresh⟩
  | @commit db db' s r snap ws v qs _ hsel hd _ hn hvw hq hr ih =>
    obtain ⟨hm, hS⟩ := ih
    have hl : Lists kl (Pg.view ts tv db) (Rows.tabs r) := by
      rw [← hkl]
      exact Pg.lists_of_selects valKind hv htv hm _ (fun t ht => Rows.tabs_nil _ t (hlen ▸ ht))
        (fun t ht => hsel t (hlen ▸ ht))
    obtain ⟨db2, hr2, hm2, hv2, -, -⟩ := Pg.compileAll_sound valKind hv htv _ _ _ hm hq
    rw [hr] at hr2
    cases hr2
    exact ⟨hm2, hv2 ▸ .commit hS hl hd hn hvw⟩
  | other _ hne htv' hq hr ih =>
    obtain ⟨hm, hS⟩ := ih
    obtain ⟨db2, hr2, hm2, -, ho, -⟩ := Pg.compileAll_sound valKind hv htv' _ _ _ hm hq
    rw [hr] at hr2
    cases hr2
    exact ⟨hm2, (ho _ (Ne.symm hne)) ▸ hS⟩

/-- An `Inv` kept by init, row reordering and every `Step` holds on PostgreSQL and on every load. -/
theorem pg_loaded_inv (hs : SqlFits A) (hf : Fits A toSt) (hv : Pg.Valid ts) (htv : valKind tv = some .int)
    (hkl : Pg.klOf ts = kl) (hlen : ts.length = {n}) (Inv : St → Prop) (hinit : Inv A.init)
    (hequiv : ∀ snap s', A.Equiv (toSt snap) s' → Inv s' → Inv (toSt snap))
    (hstep : ∀ snap ws, Step snap ws → Inv (toSt snap) → Inv (ws.val.foldl A.step (toSt snap)))
    {{db : Pg.PgDb Val}} {{s : St}} (h : PgServed A toSt ts tv Step db s) :
    Inv s ∧ ∀ r, Loads ts tv db r → decode r ⦃ o => ∃ snap, o = some snap ∧ Inv (toSt snap) ⦄ := by
  have load : ∀ {{db : Pg.PgDb Val}} {{s : St}}, PgServed A toSt ts tv Step db s → Inv s →
      ∀ r, Loads ts tv db r → decode r ⦃ o => ∃ snap, o = some snap ∧ Inv (toSt snap) ⦄ := by
    intro db s h hi r hr
    obtain ⟨hm, hS⟩ := pg_served hs hf hv htv hkl hlen h
    have hl : Lists kl (Pg.view ts tv db) (Rows.tabs r) := by
      rw [← hkl]
      exact Pg.lists_of_selects valKind hv htv hm _ (fun t ht => Rows.tabs_nil _ t (hlen ▸ ht))
        (fun t ht => hr t (hlen ▸ ht))
    apply WP.spec_mono (loaded hf (served_app hf hs hS) r hl)
    rintro o ⟨snap, rfl, he⟩
    exact ⟨snap, rfl, hequiv snap _ he hi⟩
  have hinv : Inv s := by
    induction h with
    | fresh => exact hinit
    | commit h0 hsel hd hst _ _ _ _ ih =>
      obtain ⟨snap', e, hi'⟩ := post_of_ok (load h0 ih _ hsel) hd
      cases e
      exact hstep _ _ hst hi'
    | other _ _ _ _ _ ih => exact ih
  exact ⟨hinv, load h hinv⟩

end Pg"
            );
        }
    }

    let _ = writeln!(s, "\nend {krate}.Schema");
    s
}

/// Test helper: the file at `path` must equal `text`; `H5I_APP_BLESS=1` rewrites it.
pub fn check(path: &str, text: &str) {
    if std::env::var("H5I_APP_BLESS").is_ok_and(|v| v == "1") {
        std::fs::write(path, text).unwrap();
        return;
    }
    let old = std::fs::read_to_string(path).unwrap_or_default();
    assert!(old == text, "{path} is stale; rerun with H5I_APP_BLESS=1");
}
