import H5iAppLib.Store
/-!
# PostgreSQL statements and what they do

The SQL subset `h5i-app-pgsql` compiles to: syntax (`Sql`), text (`render`),
meaning (`run`, `selected`), and the compiler's target (`createA`, `selectA`,
`compileA`), which `crates/h5i-app-pgsql/proofs` shows the extracted Rust computes.
Main results: `compile_sound`, `select_sound`.

Trusted: PostgreSQL, sent `render s` with parameters `ps`, fails or does what
`run` says, and a `SELECT` returns a reordering of `selected`. `kind` is how
the driver types a value. Text compares byte by byte, and the tables were
created by `createA` of the same schema.
-/

namespace H5iAppLib.Pg

open Classical H5iAppLib.Sql

/-- A name: the bytes of an identifier. -/
abbrev Name := List Nat

inductive Kind where
  | int | bool | text | bytes
deriving DecidableEq

structure ColDef where
  «name» : Name
  kind : Kind
  notNull : Bool

/-- A condition of a `WHERE` clause; `p` is the parameter, counted from 1. -/
inductive Cond where
  /-- `"col" = $p` -/
  | eq (col : Name) (p : Nat)
  /-- `"col" IS NOT DISTINCT FROM $p` -/
  | same (col : Name) (p : Nat)

inductive Sql where
  | create (t : Name) (cols : List ColDef) (key : List Name)
  | select (t : Name) (cols : List Name) (conds : List Cond) (order : List Name)
  | insert (t : Name) (cols conflict update : List Name)
  | delete (t : Name) (conds : List Cond)

/-! ## Text -/

/-- The bytes of an ASCII literal. -/
def lit (s : String) : List Nat := s.toList.map Char.toNat

/-- A quoted identifier: `"` around it, and each `"` inside doubled. -/
def quote (n : Name) : List Nat := 34 :: n.flatMap (fun c => if c = 34 then [34, 34] else [c]) ++ [34]

def commaSep : List (List Nat) → List Nat
  | [] => []
  | x :: xs => x ++ xs.flatMap (fun y => lit ", " ++ y)

/-- `n` in decimal. -/
def digits (n : Nat) : List Nat := if n < 10 then [48 + n] else digits (n / 10) ++ [48 + n % 10]

/-- `$p` -/
def param (p : Nat) : List Nat := 36 :: digits p

def condText : Cond → List Nat
  | .eq c p => quote c ++ lit " = " ++ param p
  | .same c p => quote c ++ lit " IS NOT DISTINCT FROM " ++ param p

def whereText : List Cond → List Nat
  | [] => []
  | c :: cs => lit " WHERE " ++ condText c ++ cs.flatMap (fun c => lit " AND " ++ condText c)

def kindText : Kind → List Nat
  | .int => lit "BIGINT"
  | .bool => lit "BOOLEAN"
  | .text => lit "TEXT"
  | .bytes => lit "BYTEA"

def colDefText (c : ColDef) : List Nat :=
  quote c.name ++ [32] ++ kindText c.kind ++ (if c.notNull then lit " NOT NULL" else []) ++ lit ", "

/-- `$1, $2, ..., $n` -/
def placeholders (n : Nat) : List Nat := commaSep ((List.range n).map (fun i => param (i + 1)))

/-- The text PostgreSQL receives for a statement. -/
def render : Sql → List Nat
  | .create t cols key =>
    lit "CREATE TABLE IF NOT EXISTS " ++ quote t ++ lit " (" ++ cols.flatMap colDefText ++
      lit "PRIMARY KEY (" ++ commaSep (key.map quote) ++ lit "))"
  | .select t cols conds order =>
    lit "SELECT " ++ commaSep (cols.map quote) ++ lit " FROM " ++ quote t ++ whereText conds ++
      (if order = [] then [] else lit " ORDER BY " ++ commaSep (order.map quote))
  | .insert t cols conflict update =>
    lit "INSERT INTO " ++ quote t ++ lit " (" ++ commaSep (cols.map quote) ++ lit ") VALUES (" ++
      placeholders cols.length ++ lit ") ON CONFLICT (" ++ commaSep (conflict.map quote) ++
      (if update = [] then lit ") DO NOTHING"
       else lit ") DO UPDATE SET " ++ commaSep (update.map (fun n => quote n ++ lit " = EXCLUDED." ++ quote n)))
  | .delete t conds => lit "DELETE FROM " ++ quote t ++ whereText conds

/-! ## A quoted name cannot end early

PostgreSQL reads a quoted identifier up to the first `"` not followed by
another `"`, and reads `""` as one `"`. Every quoted name in `render` is
followed by a byte other than `"`, so it reads back as exactly that name,
whatever bytes the name holds. -/

/-- The rest of a quoted identifier, after its opening `"`: its name and
what follows the closing `"`. -/
def lexBody : List Nat → Option (Name × List Nat)
  | [] => none
  | [c] => if c = 34 then some ([], []) else none
  | c :: d :: rest =>
    if c = 34 then
      if d = 34 then (lexBody rest).map (fun p => (34 :: p.1, p.2)) else some ([], d :: rest)
    else (lexBody (d :: rest)).map (fun p => (c :: p.1, p.2))

def lexName : List Nat → Option (Name × List Nat)
  | 34 :: rest => lexBody rest
  | _ => none

theorem lexBody_quote (n rest : List Nat) (h : rest.head? ≠ some 34) :
    lexBody (n.flatMap (fun c => if c = 34 then [34, 34] else [c]) ++ 34 :: rest) = some (n, rest) := by
  induction n with
  | nil =>
    cases rest with
    | nil => simp [lexBody]
    | cons d rs =>
      have : d ≠ 34 := by simpa using h
      simp [lexBody, this]
  | cons c n ih =>
    by_cases hc : c = 34
    · subst hc
      simp [lexBody, ih]
    · have e : n.flatMap (fun c => if c = 34 then [34, 34] else [c]) ++ 34 :: rest =
          (n.flatMap (fun c => if c = 34 then [34, 34] else [c]) ++ 34 :: rest).head! ::
          (n.flatMap (fun c => if c = 34 then [34, 34] else [c]) ++ 34 :: rest).tail := by
        cases n.flatMap (fun c => if c = 34 then [34, 34] else [c]) <;> rfl
      simp only [List.flatMap_cons, hc, if_false, List.cons_append, List.nil_append]
      rw [e, lexBody, if_neg hc, ← e, ih]
      rfl

/-- A quoted name followed by anything but `"` reads back as that name. -/
theorem lexName_quote (n rest : List Nat) (h : rest.head? ≠ some 34) :
    lexName (quote n ++ rest) = some (n, rest) := by
  simp only [quote, List.cons_append, List.append_assoc, lexName]
  exact lexBody_quote n rest h

/-! ## Meaning

A table has its column definitions, its primary key and its rows, each row a
value per column. `run` says what a statement does to the tables, or `none`
where this model makes no claim (PostgreSQL may fail there, or the statement
is outside the subset). -/

section Meaning
variable {V : Type} (kind : V → Option Kind)

structure Table (V : Type) where
  cols : List ColDef
  key : List Name
  rows : List (List V)

/-- A database: the table with each name, if any. -/
def PgDb (V : Type) := Name → Option (Table V)

def colIdx (cols : List ColDef) (n : Name) : Option Nat := cols.findIdx? (fun c => c.name = n)

/-- The value of `$p`. -/
def arg (ps : List V) (p : Nat) : Option V := if p = 0 then none else ps[p - 1]?

/-- A value can be bound where type `k` is expected: `NULL`, or of type `k`. -/
def Typed (k : Kind) (v : V) : Prop := kind v = none ∨ kind v = some k

/-- `v` can be stored in column `c`. -/
def Stores (c : ColDef) (v : V) : Prop := Typed kind c.kind v ∧ (c.notNull = true → kind v ≠ none)

/-- The condition names a column, and its parameter has the column's type. -/
def CondOk (cols : List ColDef) (ps : List V) : Cond → Prop
  | .eq n p | .same n p => ∃ i c v, colIdx cols n = some i ∧ cols[i]? = some c ∧ arg ps p = some v ∧ Typed kind c.kind v

/-- The condition is true of `row`. `=` is true only of two equal non-`NULL`
values; `IS NOT DISTINCT FROM` is equality, `NULL` included. -/
def Holds (cols : List ColDef) (ps : List V) (row : List V) : Cond → Prop
  | .eq n p => ∃ i v, colIdx cols n = some i ∧ arg ps p = some v ∧ row[i]? = some v ∧ kind v ≠ none
  | .same n p => ∃ i v, colIdx cols n = some i ∧ arg ps p = some v ∧ row[i]? = some v

def AllHold (cols : List ColDef) (ps : List V) (row : List V) (cs : List Cond) : Prop :=
  ∀ c ∈ cs, Holds kind cols ps row c

/-- The values of the named columns. -/
def project (cols : List ColDef) (ns : List Name) (row : List V) : List V :=
  ns.filterMap (fun n => (colIdx cols n).bind (row[·]?))

/-- The row an `INSERT` of all columns builds: each column gets the parameter
at its position in the column list. -/
def newRow (cols : List ColDef) (ns : List Name) (ps : List V) : Option (List V) :=
  cols.mapM (fun c => ps[ns.idxOf c.name]?)

/-- `DO UPDATE SET u = EXCLUDED.u, ...`: the columns in `up` from `new`,
the others from `old`. -/
def setCols (cols : List ColDef) (up : List Name) (old new : List V) : List V :=
  (cols.zip (old.zip new)).map (fun x => if x.1.name ∈ up then x.2.2 else x.2.1)

def put (db : PgDb V) (n : Name) (t : Table V) : PgDb V := fun n' => if n' = n then some t else db n'

/-- What a statement does to the tables. -/
noncomputable def run (db : PgDb V) : Sql → List V → Option (PgDb V)
  | .create t cols key, _ =>
    match db t with
    | some _ => some db
    | none =>
      if (cols.map (·.name)).Nodup ∧ key.Nodup ∧ ∀ n ∈ key, ∃ c ∈ cols, c.name = n ∧ c.notNull = true then
        some (put db t ⟨cols, key, []⟩)
      else none
  | .insert t ns conflict up, ps =>
    match db t with
    | none => none
    | some tab =>
      if ns.length = ps.length ∧ ns.Nodup ∧ (∀ c ∈ tab.cols, c.name ∈ ns) ∧ (∀ n ∈ ns, ∃ c ∈ tab.cols, c.name = n) ∧
          (∀ n, n ∈ conflict ↔ n ∈ tab.key) ∧ (∀ n ∈ up, ∃ c ∈ tab.cols, c.name = n) then
        match newRow tab.cols ns ps with
        | none => none
        | some new =>
          if List.Forall₂ (Stores kind) tab.cols new then
            let key := project tab.cols tab.key
            if ∃ r ∈ tab.rows, key r = key new then
              some (put db t { tab with rows := tab.rows.map (fun r =>
                if key r = key new ∧ up ≠ [] then setCols tab.cols up r new else r) })
            else some (put db t { tab with rows := tab.rows ++ [new] })
          else none
      else none
  | .delete t conds, ps =>
    match db t with
    | none => none
    | some tab =>
      if ∀ c ∈ conds, CondOk kind tab.cols ps c then
        some (put db t { tab with rows := tab.rows.filter (fun r => ¬ AllHold kind tab.cols ps r conds) })
      else none
  | .select .., _ => some db

/-- The rows a `SELECT` returns, in some order. -/
noncomputable def selected (db : PgDb V) : Sql → List V → Option (List (List V))
  | .select t ns conds _, ps =>
    match db t with
    | none => none
    | some tab =>
      if (∀ c ∈ conds, CondOk kind tab.cols ps c) ∧ ∀ n ∈ ns, ∃ c ∈ tab.cols, c.name = n then
        some ((tab.rows.filter (fun r => AllHold kind tab.cols ps r conds)).map (project tab.cols ns))
      else none
  | _, _ => none

end Meaning

/-! ## What the compiler produces

The schema the server passes to `h5i-app-pgsql`, and the statements it must
compile each planned statement and load to. -/

section Compile
variable {V : Type} (kind : V → Option Kind)

structure Col where
  «name» : Name
  kind : Kind
  nullable : Bool

/-- A tenant table: `tenant_id`, then `cols`. Its primary key is `tenant_id`
and the first `kl` columns. -/
structure Tab where
  «name» : Name
  cols : List Col
  kl : Nat

def tenantName : Name := lit "tenant_id"

/-- Every column's name, `tenant_id` first. -/
def names (t : Tab) : List Name := tenantName :: t.cols.map (·.name)

/-- A name PostgreSQL keeps as written when quoted. -/
def NameOk (n : Name) : Prop := n ≠ [] ∧ n.length ≤ 63 ∧ 0 ∉ n

/-- The prefix of the engine's own tables. -/
def Reserved (n : Name) : Prop := lit "h5i_" <+: n

def TabOk (t : Tab) : Prop :=
  NameOk t.name ∧ ¬ Reserved t.name ∧ t.cols ≠ [] ∧ t.cols.length < 1600 ∧ t.kl ≤ t.cols.length ∧
    (∀ n ∈ names t, NameOk n) ∧ (names t).Nodup

def Valid (ts : List Tab) : Prop := (∀ t ∈ ts, TabOk t) ∧ (ts.map (·.name)).Nodup

def colDefs (t : Tab) : List ColDef :=
  ⟨tenantName, .int, true⟩ :: t.cols.mapIdx (fun i c => ⟨c.name, c.kind, !c.nullable || decide (i < t.kl)⟩)

def keyNames (t : Tab) : List Name := tenantName :: (t.cols.take t.kl).map (·.name)

def createA (t : Tab) : Sql := .create t.name (colDefs t) (keyNames t)

/-- Column `i` may hold `v`: its type, and `NULL` only in a nullable column
outside the key. -/
def FitsAt (t : Tab) (i : Nat) (v : V) : Prop :=
  ∃ c, t.cols[i]? = some c ∧ Typed kind c.kind v ∧ (kind v = none → c.nullable = true ∧ t.kl ≤ i)

/-- `SELECT` of the tenant's rows of table `t`, with `f = some (i, v)` only
those whose column `i` is not distinct from `v`. -/
noncomputable def selectA (ts : List Tab) (tv : V) (t : Nat) (f : Option (Nat × V)) : Option (Sql × List V) :=
  if Valid ts then
    match ts[t]? with
    | none => none
    | some tb =>
      match f with
      | none => some (.select tb.name (tb.cols.map (·.name)) [.eq tenantName 1]
          ((tb.cols.take tb.kl).map (·.name)), [tv])
      | some (i, v) =>
        match tb.cols[i]? with
        | none => none
        | some c =>
          if Typed kind c.kind v then
            some (.select tb.name (tb.cols.map (·.name)) [.eq tenantName 1, .same c.name 2]
              ((tb.cols.take tb.kl).map (·.name)), [tv, v])
          else none
  else none

/-- The statement and parameters that run a planned statement for the tenant `tv`. -/
noncomputable def compileA (ts : List Tab) (tv : V) : AStmt V → Option (Sql × List V)
  | .up t k r =>
    if Valid ts then
      match ts[t]? with
      | none => none
      | some tb =>
        if k.length = tb.kl ∧ tb.kl + r.length = tb.cols.length ∧ (∀ i (h : i < k.length), FitsAt kind tb i k[i]) ∧
            ∀ i (h : i < r.length), FitsAt kind tb (tb.kl + i) r[i] then
          some (.insert tb.name (names tb) (keyNames tb) ((tb.cols.drop tb.kl).map (·.name)), tv :: (k ++ r))
        else none
    else none
  | .del t k =>
    if Valid ts then
      match ts[t]? with
      | none => none
      | some tb =>
        if 0 < tb.kl ∧ k.length = tb.kl ∧ ∀ i (h : i < k.length), FitsAt kind tb i k[i] then
          some (.delete tb.name (.eq tenantName 1 :: (tb.cols.take tb.kl).mapIdx (fun i c => .eq c.name (i + 2))),
            tv :: k)
        else none
    else none
  | .delWhere t i v =>
    if Valid ts then
      match ts[t]? with
      | none => none
      | some tb =>
        match tb.cols[i]? with
        | none => none
        | some c =>
          if 0 < tb.kl ∧ Typed kind c.kind v then
            some (.delete tb.name [.eq tenantName 1, .same c.name 2], [tv, v])
          else none
    else none

/-! ## The tenant's database -/

/-- Key length of table `t`. -/
def klOf (ts : List Tab) (t : Nat) : Nat := (ts[t]?.map (·.kl)).getD 0

/-- The tenant's rows of each table, without `tenant_id`. -/
noncomputable def tenantTabs (ts : List Tab) (tv : V) (db : PgDb V) : Tables V := fun t =>
  match ts[t]? with
  | none => []
  | some tb =>
    match db tb.name with
    | none => []
    | some tab => (tab.rows.filter (fun r => r.head? = some tv)).map List.tail

/-- The tenant's database, as `H5iAppLib.Sql` sees it. -/
noncomputable def view (ts : List Tab) (tv : V) (db : PgDb V) : Db V := readBack (klOf ts) (tenantTabs ts tv db)

def RowOk (tb : Tab) (r : List V) : Prop := List.Forall₂ (Stores kind) (colDefs tb) r

/-- A table as `createA` made it, with rows PostgreSQL accepted: typed, and
unique on the primary key. -/
def TableOk (tb : Tab) (tab : Table V) : Prop :=
  tab.cols = colDefs tb ∧ tab.key = keyNames tb ∧ (∀ r ∈ tab.rows, RowOk kind tb r) ∧
    (tab.rows.map (·.take (tb.kl + 1))).Nodup

/-- Every table of the schema exists, as `createA` made it. -/
def Matches (ts : List Tab) (db : PgDb V) : Prop := ∀ tb ∈ ts, ∃ tab, db tb.name = some tab ∧ TableOk kind tb tab

/-- Statements run in order; `none` if one has no claimed outcome. -/
noncomputable def runAll (db : PgDb V) : List (Sql × List V) → Option (PgDb V)
  | [] => some db
  | q :: qs => (run kind db q.1 q.2).bind (fun db' => runAll db' qs)

end Compile

/-! ## Names resolve to positions -/

section Lemmas
variable {V : Type} (kind : V → Option Kind)

theorem colDefs_names (t : Tab) : (colDefs t).map (·.name) = names t := by
  apply List.ext_getElem
  · simp [colDefs, names]
  · intro i h1 h2
    cases i with
    | zero => rfl
    | succ i => simp [colDefs, names]

theorem colDefs_length (t : Tab) : (colDefs t).length = t.cols.length + 1 := by
  simp [colDefs]

theorem names_length (t : Tab) : (names t).length = t.cols.length + 1 := by
  simp [names]

/-- In columns with distinct names, a name is found at its position. -/
theorem colIdx_of_nodup (cols : List ColDef) (h : (cols.map (·.name)).Nodup) (j : Nat) (hj : j < cols.length) :
    colIdx cols cols[j].name = some j := by
  induction cols generalizing j with
  | nil => simp at hj
  | cons c cs ih =>
    simp only [List.map_cons, List.nodup_cons, List.mem_map] at h
    cases j with
    | zero => simp [colIdx, List.findIdx?_cons]
    | succ j =>
      have hj' : j < cs.length := by simpa using hj
      have hne : ¬ c.name = cs[j].name := fun he => h.1 ⟨cs[j], List.getElem_mem hj', he.symm⟩
      have := ih h.2 j hj'
      simp only [colIdx] at this ⊢
      simp [List.findIdx?_cons, hne]
      exact this

theorem colIdx_colDefs (t : Tab) (hn : (names t).Nodup) (j : Nat) (hj : j < t.cols.length + 1) :
    colIdx (colDefs t) ((names t)[j]'(by rw [names_length]; exact hj)) = some j := by
  have hl : j < (colDefs t).length := by rw [colDefs_length]; exact hj
  have e : ((names t)[j]'(by rw [names_length]; exact hj)) = (colDefs t)[j].name := by
    simp only [← colDefs_names, List.getElem_map]
  rw [e]
  exact colIdx_of_nodup _ (by rw [colDefs_names]; exact hn) j hl

theorem names_zero (t : Tab) : ((names t)[0]'(by simp [names])) = tenantName := rfl

theorem names_succ (t : Tab) (i : Nat) (h : i < t.cols.length) :
    ((names t)[i + 1]'(by simp [names]; omega)) = t.cols[i].name := by
  simp [names]

theorem rowOk_length {tb : Tab} {r : List V} (h : RowOk kind tb r) : r.length = tb.cols.length + 1 := by
  rw [← h.length_eq, colDefs_length]

end Lemmas

/-! ## One table changes -/

section Put
variable {V : Type} (kind : V → Option Kind)

theorem klOf_some {ts : List Tab} {t : Nat} {tb : Tab} (h : ts[t]? = some tb) : klOf ts t = tb.kl := by
  simp [klOf, h]

theorem tenantTabs_some {ts : List Tab} {tv : V} {db : PgDb V} {t : Nat} {tb : Tab} {tab : Table V}
    (h : ts[t]? = some tb) (hdb : db tb.name = some tab) :
    tenantTabs ts tv db t = (tab.rows.filter (fun r => r.head? = some tv)).map List.tail := by
  simp [tenantTabs, h, hdb]

/-- Tables of a valid schema with the same name are the same table. -/
theorem index_of_name {ts : List Tab} (hv : Valid ts) {t t' : Nat} {tb tb' : Tab}
    (h : ts[t]? = some tb) (h' : ts[t']? = some tb') (hn : tb'.name = tb.name) : t' = t := by
  obtain ⟨ht, rfl⟩ := List.getElem?_eq_some_iff.1 h
  obtain ⟨ht', rfl⟩ := List.getElem?_eq_some_iff.1 h'
  have := hv.2
  exact (List.Nodup.getElem_inj_iff (l := ts.map (·.name)) this (i := t') (j := t)
    (hi := by simpa using ht') (hj := by simpa using ht)).1 (by simpa using hn)

theorem tenant_rows_keyed (tb : Tab) (tab : Table V) (tv : V) (hok : TableOk kind tb tab)
    (hkl : tb.kl ≤ tb.cols.length) :
    (((tab.rows.filter (fun r => r.head? = some tv)).map List.tail).map (·.take tb.kl)).Nodup ∧
      ∀ r ∈ (tab.rows.filter (fun r => r.head? = some tv)).map List.tail, tb.kl ≤ r.length := by
  obtain ⟨-, -, hrows, hnd⟩ := hok
  refine ⟨?_, ?_⟩
  · have h1 : ((tab.rows.filter (fun r => r.head? = some tv)).map (·.take (tb.kl + 1))).Nodup :=
      hnd.sublist (List.filter_sublist.map _)
    rw [List.map_map]
    have e : ∀ r ∈ tab.rows.filter (fun r => r.head? = some tv),
        r.take (tb.kl + 1) = tv :: ((·.take tb.kl) ∘ List.tail) r := by
      intro r hr
      have hh := (List.mem_filter.1 hr).2
      cases r with
      | nil => simp at hh
      | cons x xs => simp at hh; subst hh; simp
    rw [List.map_congr_left e] at h1
    have h2 : (((tab.rows.filter (fun r => r.head? = some tv)).map ((·.take tb.kl) ∘ List.tail)).map
        (List.cons tv)).Nodup := by
      simpa [List.map_map, Function.comp_def] using h1
    exact h2.of_map _
  · intro r hr
    obtain ⟨r0, hr0, rfl⟩ := List.mem_map.1 hr
    have := rowOk_length kind (hrows r0 (List.mem_filter.1 hr0).1)
    simp only [List.length_tail]
    omega

theorem put_self (db : PgDb V) (n : Name) (t : Table V) : put db n t n = some t := by simp [put]

theorem put_other (db : PgDb V) (n n' : Name) (t : Table V) (h : n' ≠ n) : put db n t n' = db n' := by
  simp [put, h]

/-- Replacing the rows of one table: the schema still matches, the tenant's
rows of that table change by `F`, other tables and tenants keep theirs. -/
theorem put_sound {ts : List Tab} {tv : V} {db : PgDb V} (hv : Valid ts) (hm : Matches kind ts db)
    {t : Nat} {tb : Tab} {tab : Table V} (ht : ts[t]? = some tb) (hdb : db tb.name = some tab)
    (rows' : List (List V)) (hok : TableOk kind tb { tab with rows := rows' })
    (F : List (List V) → List (List V))
    (hF : (rows'.filter (fun r => r.head? = some tv)).map List.tail =
      F ((tab.rows.filter (fun r => r.head? = some tv)).map List.tail))
    (hother : ∀ tv', tv' ≠ tv →
      rows'.filter (fun r => r.head? = some tv') = tab.rows.filter (fun r => r.head? = some tv')) :
    Matches kind ts (put db tb.name { tab with rows := rows' }) ∧
      tenantTabs ts tv (put db tb.name { tab with rows := rows' }) =
        (fun t' => if t' = t then F (tenantTabs ts tv db t) else tenantTabs ts tv db t') ∧
      (∀ tv', tv' ≠ tv → tenantTabs ts tv' (put db tb.name { tab with rows := rows' }) = tenantTabs ts tv' db) ∧
      ∀ n, n ∉ ts.map (·.name) → put db tb.name { tab with rows := rows' } n = db n := by
  have same : ∀ {t' : Nat} {tb' : Tab}, ts[t']? = some tb' → (tb'.name = tb.name ↔ t' = t) := by
    intro t' tb' h'
    constructor
    · exact fun hn => index_of_name hv ht h' hn
    · rintro rfl
      rw [ht] at h'
      rw [Option.some.inj h']
  have eq_of : ∀ {t' : Nat} {tb' : Tab}, ts[t']? = some tb' → tb'.name = tb.name → tb' = tb := by
    intro t' tb' h' hn
    have e := (same h').1 hn
    subst e
    rw [ht] at h'
    exact (Option.some.inj h').symm
  refine ⟨?_, ?_, ?_, ?_⟩
  · intro tb' hmem
    obtain ⟨t', h'⟩ := List.mem_iff_getElem?.1 hmem
    by_cases hn : tb'.name = tb.name
    · rw [eq_of h' hn]
      exact ⟨_, put_self _ _ _, hok⟩
    · rw [put_other _ _ _ _ hn]; exact hm tb' hmem
  · funext t'
    cases h' : ts[t']? with
    | none =>
      have : t' ≠ t := by rintro rfl; rw [ht] at h'; cases h'
      simp [tenantTabs, h', this]
    | some tb' =>
      by_cases hn : tb'.name = tb.name
      · have e := (same h').1 hn
        have hb := eq_of h' hn
        subst e hb
        rw [tenantTabs_some h' (put_self _ _ _), tenantTabs_some h' hdb, if_pos rfl]
        exact hF
      · have : t' ≠ t := fun e => hn ((same h').2 e)
        simp only [tenantTabs, h', put_other _ _ _ _ hn, this, if_false]
  · intro tv' hne
    funext t'
    cases h' : ts[t']? with
    | none => simp [tenantTabs, h']
    | some tb' =>
      by_cases hn : tb'.name = tb.name
      · have hb := eq_of h' hn
        subst hb
        rw [tenantTabs_some h' (put_self _ _ _), tenantTabs_some h' hdb, hother tv' hne]
      · simp only [tenantTabs, h', put_other _ _ _ _ hn]
  · intro n hn
    apply put_other
    rintro rfl
    exact hn (List.mem_map.2 ⟨tb, List.mem_of_getElem? ht, rfl⟩)

theorem tabOk_of {ts : List Tab} (hv : Valid ts) {t : Nat} {tb : Tab} (ht : ts[t]? = some tb) : TabOk tb :=
  hv.1 tb (List.mem_of_getElem? ht)

theorem wellKeyed_tenant {ts : List Tab} (hv : Valid ts) (tv : V) {db : PgDb V} (hm : Matches kind ts db) :
    WellKeyed (klOf ts) (tenantTabs ts tv db) := by
  intro t
  cases ht : ts[t]? with
  | none => simp [tenantTabs, ht]
  | some tb =>
    obtain ⟨tab, hdb, hok⟩ := hm tb (List.mem_of_getElem? ht)
    rw [tenantTabs_some ht hdb, klOf_some ht]
    exact tenant_rows_keyed kind tb tab tv hok (tabOk_of hv ht).2.2.2.2.1

theorem filter_tail (rows : List (List V)) (tv : V) (P : List V → Prop) (Q : List V → Prop)
    [DecidablePred P] [DecidablePred Q]
    (h : ∀ r ∈ rows, r.head? = some tv → (P r ↔ Q r.tail)) :
    ((rows.filter (fun r => ¬ P r)).filter (fun r => r.head? = some tv)).map List.tail =
      ((rows.filter (fun r => r.head? = some tv)).map List.tail).filter (fun x => ¬ Q x) := by
  rw [List.filter_map, List.filter_filter, List.filter_filter]
  congr 1
  apply List.filter_congr
  intro r hr
  by_cases hh : r.head? = some tv
  · simp [hh, h r hr hh]
  · simp [hh]

theorem filter_other (rows : List (List V)) (tv tv' : V) (hne : tv' ≠ tv) (P : List V → Prop) [DecidablePred P]
    (h : ∀ r ∈ rows, P r → r.head? = some tv) :
    (rows.filter (fun r => ¬ P r)).filter (fun r => r.head? = some tv') =
      rows.filter (fun r => r.head? = some tv') := by
  rw [List.filter_filter]
  apply List.filter_congr
  intro r hr
  by_cases hh : r.head? = some tv'
  · have : ¬ P r := fun hp => hne (Option.some.inj ((hh.symm.trans (h r hr hp))))
    simp [hh, this]
  · simp [hh]

theorem tableOk_sublist {tb : Tab} {tab : Table V} (hok : TableOk kind tb tab) {rows' : List (List V)}
    (hs : rows'.Sublist tab.rows) : TableOk kind tb { tab with rows := rows' } :=
  ⟨hok.1, hok.2.1, fun r hr => hok.2.2.1 r (hs.subset hr), hok.2.2.2.sublist (hs.map _)⟩

theorem colIdx_tenant {tb : Tab} (hn : (names tb).Nodup) : colIdx (colDefs tb) tenantName = some 0 :=
  colIdx_colDefs tb hn 0 (by omega)

theorem colIdx_col {tb : Tab} (hn : (names tb).Nodup) (i : Nat) (hi : i < tb.cols.length) :
    colIdx (colDefs tb) tb.cols[i].name = some (i + 1) := by
  have := colIdx_colDefs tb hn (i + 1) (by omega)
  rwa [names_succ tb i hi] at this

theorem colDefs_succ (tb : Tab) (i : Nat) (hi : i < tb.cols.length) :
    (colDefs tb)[i + 1]? = some ⟨tb.cols[i].name, tb.cols[i].kind, !tb.cols[i].nullable || decide (i < tb.kl)⟩ := by
  simp [colDefs, hi]

theorem condOk_tenant {tb : Tab} (hn : (names tb).Nodup) {tv : V} (htv : kind tv = some .int) (ps : List V) :
    CondOk kind (colDefs tb) (tv :: ps) (.eq tenantName 1) :=
  ⟨0, _, tv, colIdx_tenant hn, rfl, by simp [arg], Or.inr htv⟩

theorem holds_tenant {tb : Tab} (hn : (names tb).Nodup) {tv : V} (htv : kind tv = some .int) (ps : List V)
    (r : List V) : Holds kind (colDefs tb) (tv :: ps) r (.eq tenantName 1) ↔ r.head? = some tv := by
  simp only [Holds, colIdx_tenant hn, arg]
  constructor
  · rintro ⟨i, v, hi, hv, hr, -⟩
    cases hi; simp at hv; subst hv; rwa [List.head?_eq_getElem?]
  · intro h
    exact ⟨0, tv, rfl, by simp, by rwa [← List.head?_eq_getElem?], by simp [htv]⟩

theorem mapM_some {α β : Type} (f : α → Option β) (l : List α) (out : List β) (hl : l.length = out.length)
    (h : ∀ j (hj : j < l.length), f l[j] = some (out[j]'(hl ▸ hj))) : l.mapM f = some out := by
  induction l generalizing out with
  | nil => cases out with
    | nil => rfl
    | cons _ _ => simp at hl
  | cons a xs ih =>
    cases out with
    | nil => simp at hl
    | cons o os =>
      have h0 := h 0 (by simp)
      have ht := ih os (by simpa using hl) (fun j hj => h (j + 1) (by simp; omega))
      simp only [List.getElem_cons_zero] at h0
      simp [List.mapM_cons, h0, ht]

theorem newRow_self (tb : Tab) (hn : (names tb).Nodup) (ps : List V) (hl : ps.length = tb.cols.length + 1) :
    newRow (colDefs tb) (names tb) ps = some ps := by
  apply mapM_some _ _ _ (by rw [colDefs_length, hl])
  intro j hj
  have hj' : j < (names tb).length := by rw [names_length]; rw [colDefs_length] at hj; exact hj
  have e : (colDefs tb)[j].name = (names tb)[j] := by simp only [← colDefs_names, List.getElem_map]
  rw [e, List.Nodup.idxOf_getElem hn j hj']
  simp

/-- Projecting on the first `m` names takes the first `m` values. -/
theorem project_take (tb : Tab) (hn : (names tb).Nodup) (x : List V) (hx : x.length = tb.cols.length + 1) :
    ∀ m, m ≤ tb.cols.length + 1 → project (colDefs tb) ((names tb).take m) x = x.take m := by
  intro m
  induction m with
  | zero => intro _; simp [project]
  | succ m ih =>
    intro hm
    have hm' : m < (names tb).length := by rw [names_length]; omega
    rw [List.take_succ_eq_append_getElem hm', List.take_succ_eq_append_getElem (by omega)]
    simp only [project, List.filterMap_append] at ih ⊢
    have hc := colIdx_colDefs tb hn m (by omega)
    rw [ih (by omega)]
    simp [hc, List.getElem?_eq_getElem (show m < x.length by omega)]

theorem keyNames_take (tb : Tab) : keyNames tb = (names tb).take (tb.kl + 1) := by
  simp [keyNames, names, List.map_take]

theorem project_key (tb : Tab) (hn : (names tb).Nodup) (hkl : tb.kl ≤ tb.cols.length) (x : List V)
    (hx : x.length = tb.cols.length + 1) : project (colDefs tb) (keyNames tb) x = x.take (tb.kl + 1) := by
  rw [keyNames_take]; exact project_take tb hn x hx _ (by omega)

/-- On a key conflict, `DO UPDATE SET` of the non-key columns gives the new row. -/
theorem setCols_eq (tb : Tab) (hn : (names tb).Nodup) (x new : List V)
    (hx : x.length = tb.cols.length + 1) (hnew : new.length = tb.cols.length + 1)
    (hk : x.take (tb.kl + 1) = new.take (tb.kl + 1)) :
    setCols (colDefs tb) ((tb.cols.drop tb.kl).map (·.name)) x new = new := by
  have hup : (tb.cols.drop tb.kl).map (·.name) = (names tb).drop (tb.kl + 1) := by
    simp [names, List.map_drop]
  rw [hup]
  apply List.ext_getElem
  · simp [setCols, colDefs_length, hx, hnew]
  · intro j h1 h2
    simp only [setCols, List.getElem_map, List.getElem_zip]
    have hj : j < (names tb).length := by rw [names_length]; omega
    have e : ((colDefs tb)[j]'(by rw [colDefs_length]; omega)).name = (names tb)[j] := by
      simp only [← colDefs_names, List.getElem_map]
    rw [e]
    by_cases hjk : tb.kl + 1 ≤ j
    · rw [if_pos]
      rw [List.mem_iff_getElem]
      exact ⟨j - (tb.kl + 1), by simp; omega, by simp; congr 1; omega⟩
    · rw [if_neg]
      · have := congrArg (·[j]?) hk
        simp only [List.getElem?_take, if_pos (show j < tb.kl + 1 by omega)] at this
        rw [List.getElem?_eq_getElem (by omega), List.getElem?_eq_getElem (by omega)] at this
        exact Option.some.inj this
      · rw [List.mem_iff_getElem]
        rintro ⟨i, hi, he⟩
        simp only [List.getElem_drop] at he
        have := (List.Nodup.getElem_inj_iff hn).1 he
        simp at hi; omega

theorem upsert_found {α κ : Type} [DecidableEq κ] (key : α → κ) (a : α) (l : List α) (hn : (l.map key).Nodup)
    (h : ∃ y ∈ l, key y = key a) : upsert key a l = l.map (fun y => if key y = key a then a else y) := by
  induction l with
  | nil => simp at h
  | cons y ys ih =>
    simp only [List.map_cons, List.nodup_cons, List.mem_map] at hn
    unfold upsert
    by_cases hy : key y = key a
    · have e : ys.map (fun y => if key y = key a then a else y) = ys := by
        refine (List.map_congr_left (g := id) (fun z hz => ?_)).trans (List.map_id ys)
        have : key z ≠ key a := fun he => hn.1 ⟨z, hz, he.trans hy.symm⟩
        simp [this]
      simp only [hy, if_true, List.map_cons, e]
    · simp only [hy, if_false, List.map_cons]
      rw [ih hn.2]
      obtain ⟨z, hz, he⟩ := h
      rcases List.mem_cons.1 hz with rfl | hz
      · exact absurd he hy
      · exact ⟨z, hz, he⟩

theorem upsert_new {α κ : Type} [DecidableEq κ] (key : α → κ) (a : α) (l : List α)
    (h : ∀ y ∈ l, key y ≠ key a) : upsert key a l = l ++ [a] := by
  induction l with
  | nil => rfl
  | cons y ys ih =>
    unfold upsert
    simp [h y List.mem_cons_self, ih (fun z hz => h z (List.mem_cons_of_mem _ hz))]

end Put

/-! ## Each compiled statement does what `exec` says -/

section Sound
variable {V : Type} (kind : V → Option Kind)

/-- What a compiled statement does, on the tables: it changes the tenant's
rows as `applyW` does, and nothing else. -/
def Effect (ts : List Tab) (tv : V) (db db' : PgDb V) (w : AWrite V) : Prop :=
  Matches kind ts db' ∧ tenantTabs ts tv db' = applyW (klOf ts) (tenantTabs ts tv db) w ∧
    (∀ tv', tv' ≠ tv → tenantTabs ts tv' db' = tenantTabs ts tv' db) ∧
    ∀ n, n ∉ ts.map (·.name) → db' n = db n

theorem del_sound {ts : List Tab} {tv : V} {db : PgDb V} (hv : Valid ts) (htv : kind tv = some .int)
    (hm : Matches kind ts db) {t : Nat} {k : List V} {q : Sql} {ps : List V}
    (hc : compileA kind ts tv (.del t k) = some (q, ps)) :
    ∃ db', run kind db q ps = some db' ∧ Effect kind ts tv db db' (.del t k) ∧ 0 < klOf ts t := by
  simp only [compileA, if_pos hv] at hc
  cases ht : ts[t]? with
  | none => simp [ht] at hc
  | some tb =>
    simp only [ht] at hc
    split at hc
    · rename_i hcond
      obtain ⟨hkl0, hlen, hfit⟩ := hcond
      simp only [Option.some.injEq, Prod.mk.injEq] at hc
      obtain ⟨rfl, rfl⟩ := hc
      obtain ⟨tab, hdb, hok⟩ := hm tb (List.mem_of_getElem? ht)
      have tok := tabOk_of hv ht
      have hn := tok.2.2.2.2.2.2
      have hkl := tok.2.2.2.2.1
      obtain ⟨hcols, hkey, hrows, hnd⟩ := hok
      have hki : ∀ i (hi : i < k.length), i < tb.cols.length ∧ kind k[i] ≠ none ∧
          (colDefs tb)[i + 1]? = some ⟨tb.cols[i].name, tb.cols[i].kind, !tb.cols[i].nullable || decide (i < tb.kl)⟩ ∧
          Typed kind tb.cols[i].kind k[i] := by
        intro i hi
        have hic : i < tb.cols.length := by omega
        obtain ⟨c, hc, hty, hnull⟩ := hfit i hi
        rw [List.getElem?_eq_getElem hic, Option.some.injEq] at hc
        subst hc
        exact ⟨hic, fun h0 => by have := (hnull h0).2; omega, colDefs_succ tb i hic, hty⟩
      -- The conditions: the tenant, then each key column.
      have hmem : ∀ c ∈ (tb.cols.take tb.kl).mapIdx (fun i c => Cond.eq c.name (i + 2)),
          ∃ i, ∃ hi : i < k.length, c = .eq tb.cols[i].name (i + 2) := by
        intro c hc
        obtain ⟨i, hi, rfl⟩ := List.mem_mapIdx.1 hc
        have hi' : i < k.length := by simp at hi; omega
        exact ⟨i, hi', by simp⟩
      have hok' : ∀ c ∈ Cond.eq tenantName 1 :: (tb.cols.take tb.kl).mapIdx (fun i c => Cond.eq c.name (i + 2)),
          CondOk kind tab.cols (tv :: k) c := by
        intro c hc
        rw [hcols]
        rcases List.mem_cons.1 hc with rfl | hc
        · exact condOk_tenant kind hn htv k
        · obtain ⟨i, hi, rfl⟩ := hmem c hc
          obtain ⟨hic, -, hd, hty⟩ := hki i hi
          exact ⟨i + 1, _, k[i], colIdx_col hn i hic, hd, by simp [arg, hi], hty⟩
      -- On the tenant's rows, the conditions say the key is `k`.
      have hall : ∀ r ∈ tab.rows, r.head? = some tv →
          (AllHold kind tab.cols (tv :: k) r (Cond.eq tenantName 1 :: (tb.cols.take tb.kl).mapIdx
            (fun i c => Cond.eq c.name (i + 2))) ↔ r.tail.take tb.kl = k) := by
        intro r hr hh
        have hrl := rowOk_length kind (hrows r hr)
        rw [hcols]
        constructor
        · intro hA
          apply List.ext_getElem?
          intro i
          by_cases hi : i < k.length
          · have hc : Cond.eq tb.cols[i].name (i + 2) ∈ (tb.cols.take tb.kl).mapIdx (fun i c => Cond.eq c.name (i + 2)) := by
              apply List.mem_mapIdx.2
              exact ⟨i, by simp; omega, by simp⟩
            obtain ⟨j, v, hj, hv', hrv, -⟩ := hA _ (List.mem_cons_of_mem _ hc)
            rw [colIdx_col hn i (hki i hi).1] at hj
            cases hj
            have hv2 : k[i]? = some v := by simpa [arg] using hv'
            rw [hv2, List.getElem?_take, if_pos (by omega), List.getElem?_tail, hrv]
          · rw [List.getElem?_eq_none (by simp; omega), List.getElem?_eq_none (by omega)]
        · intro hk c hc
          rcases List.mem_cons.1 hc with rfl | hc
          · exact (holds_tenant kind hn htv k r).2 hh
          · obtain ⟨i, hi, rfl⟩ := hmem c hc
            refine ⟨i + 1, k[i], colIdx_col hn i (hki i hi).1, by simp [arg, hi], ?_, (hki i hi).2.1⟩
            have := congrArg (·[i]?) hk
            simp only [List.getElem?_take, if_pos (show i < tb.kl by omega), List.getElem?_tail] at this
            rw [this, List.getElem?_eq_getElem hi]
      have htenant : ∀ r ∈ tab.rows, AllHold kind tab.cols (tv :: k) r (Cond.eq tenantName 1 ::
          (tb.cols.take tb.kl).mapIdx (fun i c => Cond.eq c.name (i + 2))) → r.head? = some tv := by
        intro r _ hA
        have := hA _ List.mem_cons_self
        rw [hcols] at this
        exact (holds_tenant kind hn htv k r).1 this
      refine ⟨_, by simp only [run, hdb]; rw [if_pos hok'], ?_⟩
      obtain ⟨h1, h2, h3, h4⟩ := put_sound kind hv hm ht hdb _
        (tableOk_sublist kind ⟨hcols, hkey, hrows, hnd⟩ List.filter_sublist)
        (fun l => l.filter (fun x => ¬ x.take tb.kl = k))
        (filter_tail _ tv _ _ hall)
        (fun tv' hne => filter_other _ tv tv' hne _ htenant)
      refine ⟨⟨h1, ?_, h3, h4⟩, by rw [klOf_some ht]; exact hkl0⟩
      rw [h2]
      funext t'
      simp only [applyW, klOf_some ht]
    · simp at hc

theorem delWhere_sound {ts : List Tab} {tv : V} {db : PgDb V} (hv : Valid ts) (htv : kind tv = some .int)
    (hm : Matches kind ts db) {t i : Nat} {v : V} {q : Sql} {ps : List V}
    (hc : compileA kind ts tv (.delWhere t i v) = some (q, ps)) :
    ∃ db', run kind db q ps = some db' ∧ Effect kind ts tv db db' (.delWhere t i v) ∧ 0 < klOf ts t := by
  simp only [compileA, if_pos hv] at hc
  cases ht : ts[t]? with
  | none => simp [ht] at hc
  | some tb =>
    simp only [ht] at hc
    cases hci : tb.cols[i]? with
    | none => simp [hci] at hc
    | some c =>
      simp only [hci] at hc
      split at hc
      · rename_i hcond
        obtain ⟨hkl0, hty⟩ := hcond
        simp only [Option.some.injEq, Prod.mk.injEq] at hc
        obtain ⟨rfl, rfl⟩ := hc
        obtain ⟨tab, hdb, hcols, hkey, hrows, hnd⟩ := hm tb (List.mem_of_getElem? ht)
        have hn := (tabOk_of hv ht).2.2.2.2.2.2
        obtain ⟨hic, rfl⟩ := List.getElem?_eq_some_iff.1 hci
        have hcol := colIdx_col hn i hic
        have hok' : ∀ d ∈ [Cond.eq tenantName 1, Cond.same tb.cols[i].name 2], CondOk kind tab.cols [tv, v] d := by
          intro d hd
          rw [hcols]
          simp only [List.mem_cons, List.not_mem_nil, or_false] at hd
          rcases hd with rfl | rfl
          · exact condOk_tenant kind hn htv [v]
          · exact ⟨i + 1, _, v, hcol, colDefs_succ tb i hic, by simp [arg], hty⟩
        have hall : ∀ r ∈ tab.rows, r.head? = some tv →
            (AllHold kind tab.cols [tv, v] r [Cond.eq tenantName 1, Cond.same tb.cols[i].name 2] ↔
              r.tail[i]? = some v) := by
          intro r _ hh
          have e : AllHold kind (colDefs tb) [tv, v] r [Cond.eq tenantName 1, Cond.same tb.cols[i].name 2] ↔
              Holds kind (colDefs tb) [tv, v] r (Cond.eq tenantName 1) ∧
                Holds kind (colDefs tb) [tv, v] r (Cond.same tb.cols[i].name 2) := by
            simp [AllHold]
          rw [hcols, e, holds_tenant kind hn htv [v] r, List.getElem?_tail]
          simp only [hh, true_and, Holds, hcol]
          constructor
          · rintro ⟨j, w, hj, hw, hr⟩
            cases hj; simp [arg] at hw; subst hw; exact hr
          · intro hr
            exact ⟨i + 1, v, rfl, by simp [arg], hr⟩
        have htenant : ∀ r ∈ tab.rows,
            AllHold kind tab.cols [tv, v] r [Cond.eq tenantName 1, Cond.same tb.cols[i].name 2] → r.head? = some tv := by
          intro r _ hA
          have := hA _ List.mem_cons_self
          rw [hcols] at this
          exact (holds_tenant kind hn htv [v] r).1 this
        refine ⟨_, by simp only [run, hdb]; rw [if_pos hok'], ?_⟩
        obtain ⟨h1, h2, h3, h4⟩ := put_sound kind hv hm ht hdb _
          (tableOk_sublist kind ⟨hcols, hkey, hrows, hnd⟩ List.filter_sublist)
          (fun l => l.filter (fun x => ¬ x[i]? = some v))
          (filter_tail _ tv _ _ hall)
          (fun tv' hne => filter_other _ tv tv' hne _ htenant)
        refine ⟨⟨h1, ?_, h3, h4⟩, by rw [klOf_some ht]; exact hkl0⟩
        rw [h2]
        funext t'
        simp only [applyW]
      · simp at hc

theorem stores_row (tb : Tab) {tv : V} (htv : kind tv = some .int) (row : List V)
    (hl : row.length = tb.cols.length) (hfit : ∀ i (hi : i < row.length), FitsAt kind tb i row[i]) :
    RowOk kind tb (tv :: row) := by
  rw [RowOk, List.forall₂_iff_get]
  refine ⟨by simp [colDefs_length, hl], fun j h1 h2 => ?_⟩
  simp only [List.get_eq_getElem]
  cases j with
  | zero => exact ⟨Or.inr htv, fun _ => by simp [htv]⟩
  | succ i =>
    have hi : i < row.length := by simp at h2; omega
    obtain ⟨c, hc, hty, hnull⟩ := hfit i hi
    have hic : i < tb.cols.length := by omega
    rw [List.getElem?_eq_getElem hic, Option.some.injEq] at hc
    subst hc
    have e : (colDefs tb)[i + 1] = ⟨tb.cols[i].name, tb.cols[i].kind, !tb.cols[i].nullable || decide (i < tb.kl)⟩ := by
      simp [colDefs]
    rw [e]
    refine ⟨by simpa using hty, fun hnn h0 => ?_⟩
    obtain ⟨h3, h4⟩ := hnull (by simpa using h0)
    simp [h3] at hnn; omega

theorem up_sound {ts : List Tab} {tv : V} {db : PgDb V} (hv : Valid ts) (htv : kind tv = some .int)
    (hm : Matches kind ts db) {t : Nat} {k r : List V} {q : Sql} {ps : List V}
    (hc : compileA kind ts tv (.up t k r) = some (q, ps)) :
    ∃ db', run kind db q ps = some db' ∧ Effect kind ts tv db db' (.put t (klOf ts t) (k ++ r)) ∧
      k.length = klOf ts t := by
  simp only [compileA, if_pos hv] at hc
  cases ht : ts[t]? with
  | none => simp [ht] at hc
  | some tb =>
    simp only [ht] at hc
    split at hc
    · rename_i hcond
      obtain ⟨hlen, hlen2, hfk, hfr⟩ := hcond
      simp only [Option.some.injEq, Prod.mk.injEq] at hc
      obtain ⟨rfl, rfl⟩ := hc
      obtain ⟨tab, hdb, hcols, hkey, hrows, hnd⟩ := hm tb (List.mem_of_getElem? ht)
      have tok := tabOk_of hv ht
      have hn := tok.2.2.2.2.2.2
      have hkl := tok.2.2.2.2.1
      rw [klOf_some ht]
      have hrl : (k ++ r).length = tb.cols.length := by simp; omega
      have hfit : ∀ i (hi : i < (k ++ r).length), FitsAt kind tb i (k ++ r)[i] := by
        intro i hi
        by_cases hik : i < k.length
        · rw [List.getElem_append_left hik]; exact hfk i hik
        · rw [List.getElem_append_right (by omega)]
          have := hfr (i - k.length) (by simp at hi; omega)
          rwa [show tb.kl + (i - k.length) = i by omega] at this
      have hnewok := stores_row kind tb htv (k ++ r) hrl hfit
      have hlnew : (tv :: (k ++ r)).length = tb.cols.length + 1 := by simp [hrl]
      have hkeynew : (tv :: (k ++ r)).take (tb.kl + 1) = tv :: k := by
        simp [← hlen]
      have hrowlen : ∀ x ∈ tab.rows, x.length = tb.cols.length + 1 := fun x hx => rowOk_length kind (hrows x hx)
      -- The key the conflict clause compares.
      have hkeyof : ∀ x : List V, x.length = tb.cols.length + 1 →
          project tab.cols tab.key x = x.take (tb.kl + 1) := by
        intro x hx; rw [hcols, hkey]; exact project_key tb hn hkl x hx
      have hchecks : (names tb).length = (tv :: (k ++ r)).length ∧ (names tb).Nodup ∧
          (∀ c ∈ tab.cols, c.name ∈ names tb) ∧ (∀ n ∈ names tb, ∃ c ∈ tab.cols, c.name = n) ∧
          (∀ n, n ∈ keyNames tb ↔ n ∈ tab.key) ∧
          (∀ n ∈ (tb.cols.drop tb.kl).map (·.name), ∃ c ∈ tab.cols, c.name = n) := by
        rw [hcols, hkey]
        refine ⟨by rw [names_length, hlnew], hn, fun c hc => ?_, fun n hn' => ?_, fun _ => Iff.rfl, fun n hn' => ?_⟩
        · rw [← colDefs_names]; exact List.mem_map_of_mem hc
        · rw [← colDefs_names] at hn'; obtain ⟨c, hc, rfl⟩ := List.mem_map.1 hn'; exact ⟨c, hc, rfl⟩
        · have : n ∈ names tb := by
            simp only [names, List.mem_cons]; right; exact List.map_subset _ (List.drop_subset _ _) hn'
          rw [← colDefs_names] at this; obtain ⟨c, hc, rfl⟩ := List.mem_map.1 this; exact ⟨c, hc, rfl⟩
      have hnr : newRow tab.cols (names tb) (tv :: (k ++ r)) = some (tv :: (k ++ r)) := by
        rw [hcols]; exact newRow_self tb hn _ hlnew
      have hst : List.Forall₂ (Stores kind) tab.cols (tv :: (k ++ r)) := by rw [hcols]; exact hnewok
      -- The tenant's rows, and a row's key among them.
      have htk : ∀ x ∈ tab.rows, (x.take (tb.kl + 1) = tv :: k ↔ x.head? = some tv ∧ x.tail.take tb.kl = k) := by
        intro x hx
        have := hrowlen x hx
        cases x with
        | nil => simp at this
        | cons y ys => simp [List.take_succ_cons]
      have hwk := tenant_rows_keyed kind tb tab tv ⟨hcols, hkey, hrows, hnd⟩ hkl
      by_cases hex : ∃ x ∈ tab.rows, project tab.cols tab.key x = project tab.cols tab.key (tv :: (k ++ r))
      · -- A conflict: the row with the key becomes the new row.
        have hg : ∀ x ∈ tab.rows, (if project tab.cols tab.key x = project tab.cols tab.key (tv :: (k ++ r)) ∧
            (tb.cols.drop tb.kl).map (·.name) ≠ [] then setCols tab.cols ((tb.cols.drop tb.kl).map (·.name)) x (tv :: (k ++ r)) else x) =
            if x.take (tb.kl + 1) = tv :: k then tv :: (k ++ r) else x := by
          intro x hx
          rw [hkeyof x (hrowlen x hx), hkeyof _ hlnew, hkeynew]
          by_cases hxk : x.take (tb.kl + 1) = tv :: k
          · rw [if_pos hxk]
            by_cases hup : (tb.cols.drop tb.kl).map (·.name) = []
            · rw [if_neg (fun h => h.2 hup)]
              have hcl : tb.cols.length ≤ tb.kl := by simpa using hup
              rw [← List.take_of_length_le (show x.length ≤ tb.kl + 1 by rw [hrowlen x hx]; omega), hxk, ← hkeynew,
                List.take_of_length_le (by rw [hlnew]; omega)]
            · rw [if_pos ⟨hxk, hup⟩, hcols]
              exact setCols_eq tb hn x _ (hrowlen x hx) hlnew (hxk.trans hkeynew.symm)
          · rw [if_neg (fun h => hxk h.1), if_neg hxk]
        let g : List V → List V := fun x => if x.take (tb.kl + 1) = tv :: k then tv :: (k ++ r) else x
        have hrows' : tab.rows.map (fun x => if project tab.cols tab.key x = project tab.cols tab.key (tv :: (k ++ r)) ∧
            (tb.cols.drop tb.kl).map (·.name) ≠ [] then setCols tab.cols ((tb.cols.drop tb.kl).map (·.name)) x (tv :: (k ++ r)) else x) =
            tab.rows.map g := List.map_congr_left hg
        refine ⟨put db tb.name { tab with rows := tab.rows.map g }, ?_, ?_, hlen⟩
        · simp only [run, hdb]
          rw [if_pos hchecks, hnr]
          simp only
          rw [if_pos hst, if_pos hex, hrows']
        have hgk : ∀ x, (g x).take (tb.kl + 1) = x.take (tb.kl + 1) := by
          intro x; simp only [g]; split
          · rename_i h; rw [hkeynew, h]
          · rfl
        have hgh : ∀ x, (g x).head? = x.head? := by
          intro x; simp only [g]; split
          · rename_i h
            have := congrArg List.head? h
            rw [List.head?_take] at this
            simp at this; simp [this]
          · rfl
        have hok' : TableOk kind tb { tab with rows := tab.rows.map g } := by
          refine ⟨hcols, hkey, fun x hx => ?_, ?_⟩
          · obtain ⟨y, hy, rfl⟩ := List.mem_map.1 hx
            simp only [g]; split
            · exact hnewok
            · exact hrows y hy
          · rw [List.map_map]
            have : ((fun x : List V => x.take (tb.kl + 1)) ∘ g) = fun x => x.take (tb.kl + 1) := funext hgk
            rw [this]; exact hnd
        obtain ⟨h1, h2, h3, h4⟩ := put_sound kind (tv := tv) hv hm ht hdb _ hok'
          (fun l : List (List V) => l.map (fun y : List V => if y.take tb.kl = k then k ++ r else y))
          (by
            rw [List.filter_map, List.map_map, List.map_map]
            have e1 : ((fun r : List V => decide (r.head? = some tv)) ∘ g) = fun r => decide (r.head? = some tv) := by
              funext x; simp [hgh]
            rw [e1]
            apply List.map_congr_left
            intro x hx
            have hh := (List.mem_filter.1 hx).2
            have hxr := (List.mem_filter.1 hx).1
            simp only [decide_eq_true_eq] at hh
            simp only [Function.comp, g]
            by_cases hxk : x.take (tb.kl + 1) = tv :: k
            · rw [if_pos hxk, if_pos ((htk x hxr).1 hxk).2]; rfl
            · rw [if_neg hxk, if_neg (fun h => hxk ((htk x hxr).2 ⟨hh, h⟩))])
          (fun tv' hne => by
            rw [List.filter_map]
            have e1 : ((fun r : List V => decide (r.head? = some tv')) ∘ g) = fun r => decide (r.head? = some tv') := by
              funext x; simp [hgh]
            rw [e1]
            refine (List.map_congr_left (g := id) (fun x hx => ?_)).trans (List.map_id _)
            have hh := (List.mem_filter.1 hx).2
            simp only [decide_eq_true_eq] at hh
            simp only [g, id]
            rw [if_neg]
            intro hxk
            have := congrArg List.head? hxk
            rw [List.head?_take] at this
            simp [hh] at this
            exact hne this)
        refine ⟨h1, ?_, h3, h4⟩
        rw [h2]
        funext t'
        simp only [applyW, klOf_some ht]
        split
        · rename_i e
          subst e
          have hkk : (k ++ r).take tb.kl = k := by
            rw [List.take_append_of_le_length (by omega), List.take_of_length_le (by omega)]
          rw [tenantTabs_some ht hdb, upsert_found _ _ _ hwk.1]
          · simp only [hkk]
          · obtain ⟨x, hx, hxk⟩ := hex
            rw [hkeyof x (hrowlen x hx), hkeyof _ hlnew, hkeynew] at hxk
            obtain ⟨hh, hk⟩ := (htk x hx).1 hxk
            refine ⟨x.tail, List.mem_map.2 ⟨x, List.mem_filter.2 ⟨hx, by simpa using hh⟩, rfl⟩, ?_⟩
            rw [hk, hkk]
        · rfl
      · -- No conflict: the new row is appended.
        refine ⟨put db tb.name { tab with rows := tab.rows ++ [tv :: (k ++ r)] }, ?_, ?_, hlen⟩
        · simp only [run, hdb]
          rw [if_pos hchecks, hnr]
          simp only
          rw [if_pos hst, if_neg hex]
        have hnot : ∀ x ∈ tab.rows, ¬ x.take (tb.kl + 1) = tv :: k := by
          intro x hx hxk
          apply hex
          refine ⟨x, hx, ?_⟩
          rw [hkeyof x (hrowlen x hx), hkeyof _ hlnew, hkeynew, hxk]
        have hok' : TableOk kind tb { tab with rows := tab.rows ++ [tv :: (k ++ r)] } := by
          refine ⟨hcols, hkey, fun x hx => ?_, ?_⟩
          · rcases List.mem_append.1 hx with hx | hx
            · exact hrows x hx
            · simp at hx; subst hx; exact hnewok
          · rw [List.map_append]
            apply List.nodup_append.2
            refine ⟨hnd, by simp, ?_⟩
            intro a ha b hb hab
            simp at hb; subst hb
            obtain ⟨x, hx, rfl⟩ := List.mem_map.1 ha
            exact hnot x hx (hab.trans hkeynew)
        obtain ⟨h1, h2, h3, h4⟩ := put_sound kind (tv := tv) hv hm ht hdb _ hok'
          (fun l : List (List V) => l ++ [k ++ r])
          (by simp [List.filter_append])
          (fun tv' hne => by
            rw [List.filter_append]
            have : ¬ tv = tv' := fun h => hne h.symm
            simp [this])
        refine ⟨h1, ?_, h3, h4⟩
        rw [h2]
        funext t'
        simp only [applyW, klOf_some ht]
        split
        · rename_i e
          subst e
          rw [tenantTabs_some ht hdb, upsert_new]
          intro y hy hyk
          obtain ⟨x, hx, rfl⟩ := List.mem_map.1 hy
          have hh := (List.mem_filter.1 hx).2
          simp only [decide_eq_true_eq] at hh
          apply hnot x (List.mem_filter.1 hx).1
          rw [(htk x (List.mem_filter.1 hx).1).2]
          refine ⟨hh, ?_⟩
          rw [hyk, List.take_append_of_le_length (by omega), List.take_of_length_le (by omega)]
        · rfl
    · simp at hc

/-- The view after an effect: `exec`, for this tenant; the same, for others. -/
theorem effect_view {ts : List Tab} (hv : Valid ts) {tv : V} {db db' : PgDb V} (hm : Matches kind ts db)
    {w : AWrite V} (he : Effect kind ts tv db db' w) (hw : WriteOk (klOf ts) w) :
    view ts tv db' = exec (view ts tv db) (planA w) ∧ ∀ tv', tv' ≠ tv → view ts tv' db' = view ts tv' db := by
  obtain ⟨-, h2, h3, -⟩ := he
  refine ⟨?_, fun tv' hne => by simp only [view, h3 tv' hne]⟩
  simp only [view, h2]
  exact (step_sound (klOf ts) _ w (wellKeyed_tenant kind hv tv hm) hw).1

/-- A compiled statement does to the tenant's database what `exec` says and
touches no other tenant or table. -/
theorem compile_sound {ts : List Tab} {tv : V} {db : PgDb V} (hv : Valid ts) (htv : kind tv = some .int)
    (hm : Matches kind ts db) {s : AStmt V} {q : Sql} {ps : List V}
    (hc : compileA kind ts tv s = some (q, ps)) :
    ∃ db', run kind db q ps = some db' ∧ Matches kind ts db' ∧ view ts tv db' = exec (view ts tv db) s ∧
      (∀ tv', tv' ≠ tv → view ts tv' db' = view ts tv' db) ∧ ∀ n, n ∉ ts.map (·.name) → db' n = db n := by
  cases s with
  | up t k r =>
    obtain ⟨db', hr, he, hk⟩ := up_sound kind hv htv hm hc
    have hw : WriteOk (klOf ts) (.put t (klOf ts t) (k ++ r)) := ⟨rfl, by simp; omega⟩
    obtain ⟨h1, h2⟩ := effect_view kind hv hm he hw
    have e : planA (AWrite.put t (klOf ts t) (k ++ r)) = .up t k r := by
      simp [planA, ← hk, List.take_left', List.drop_left']
    exact ⟨db', hr, he.1, e ▸ h1, h2, he.2.2.2⟩
  | del t k =>
    obtain ⟨db', hr, he, hk⟩ := del_sound kind hv htv hm hc
    obtain ⟨h1, h2⟩ := effect_view kind hv hm he hk
    exact ⟨db', hr, he.1, h1, h2, he.2.2.2⟩
  | delWhere t i v =>
    obtain ⟨db', hr, he, hk⟩ := delWhere_sound kind hv htv hm hc
    obtain ⟨h1, h2⟩ := effect_view kind hv hm he hk
    exact ⟨db', hr, he.1, h1, h2, he.2.2.2⟩

/-- Compiled statements, run in order, do what `execAll` says. -/
theorem compileAll_sound {ts : List Tab} {tv : V} (hv : Valid ts) (htv : kind tv = some .int) :
    ∀ (ss : List (AStmt V)) (qs : List (Sql × List V)) (db : PgDb V), Matches kind ts db →
      ss.mapM (compileA kind ts tv) = some qs →
      ∃ db', runAll kind db qs = some db' ∧ Matches kind ts db' ∧ view ts tv db' = execAll (view ts tv db) ss ∧
        (∀ tv', tv' ≠ tv → view ts tv' db' = view ts tv' db) ∧ ∀ n, n ∉ ts.map (·.name) → db' n = db n := by
  intro ss
  induction ss with
  | nil =>
    intro qs db hm h
    simp at h; subst h
    exact ⟨db, rfl, hm, rfl, fun _ _ => rfl, fun _ _ => rfl⟩
  | cons s ss ih =>
    intro qs db hm h
    cases hq : compileA kind ts tv s with
    | none => simp [List.mapM_cons, hq] at h
    | some q =>
    cases hqs : ss.mapM (compileA kind ts tv) with
    | none => simp [List.mapM_cons, hq, hqs] at h
    | some qs' =>
    simp [List.mapM_cons, hq, hqs] at h
    subst h
    obtain ⟨db1, hr1, hm1, hv1, ho1, hn1⟩ := compile_sound kind hv htv hm hq
    obtain ⟨db2, hr2, hm2, hv2, ho2, hn2⟩ := ih qs' db1 hm1 hqs
    refine ⟨db2, by simp [runAll, hr1, hr2], hm2, ?_, fun tv' hne => (ho2 tv' hne).trans (ho1 tv' hne),
      fun n hn => (hn2 n hn).trans (hn1 n hn)⟩
    rw [hv2, hv1]; rfl

/-- Selecting every column but `tenant_id` drops the `tenant_id` value. -/
theorem project_cols (tb : Tab) (hn : (names tb).Nodup) (x : List V) (hx : x.length = tb.cols.length + 1) :
    project (colDefs tb) (tb.cols.map (·.name)) x = x.tail := by
  have h := project_take tb hn x hx (tb.cols.length + 1) (le_refl _)
  rw [List.take_of_length_le (by rw [names_length]), List.take_of_length_le (by omega)] at h
  have hc := colIdx_tenant (tb := tb) hn
  cases x with
  | nil => simp at hx
  | cons y ys =>
    simp only [names, project, List.filterMap_cons, hc] at h
    simp only [Option.bind_some, List.getElem?_cons_zero, List.cons.injEq] at h
    exact h.2

/-- The rows a `SELECT` with this filter keeps. -/
def Filter : Option (Nat × V) → List V → Prop
  | none, _ => True
  | some (i, v), row => ColIs i v row

/-- A compiled `SELECT` returns the tenant's rows that pass the filter, once
each (`Sel`). -/
theorem select_sound {ts : List Tab} {tv : V} {db : PgDb V} (hv : Valid ts) (htv : kind tv = some .int)
    (hm : Matches kind ts db) {t : Nat} {f : Option (Nat × V)} {q : Sql} {ps : List V}
    (hc : selectA kind ts tv t f = some (q, ps)) :
    ∃ R0, selected kind db q ps = some R0 ∧
      ∀ R, R.Perm R0 → Sel (klOf ts) (view ts tv db) t (Filter f) R := by
  simp only [selectA, if_pos hv] at hc
  cases ht : ts[t]? with
  | none => simp [ht] at hc
  | some tb =>
    simp only [ht] at hc
    obtain ⟨tab, hdb, hcols, hkey, hrows, hnd⟩ := hm tb (List.mem_of_getElem? ht)
    have hn := (tabOk_of hv ht).2.2.2.2.2.2
    have hkl := (tabOk_of hv ht).2.2.2.2.1
    have hrowlen : ∀ x ∈ tab.rows, x.length = tb.cols.length + 1 := fun x hx => rowOk_length kind (hrows x hx)
    have hwk := tenant_rows_keyed kind tb tab tv ⟨hcols, hkey, hrows, hnd⟩ hkl
    have hL := tenantTabs_some (tv := tv) ht hdb
    have hns : ∀ n ∈ tb.cols.map (·.name), ∃ c ∈ tab.cols, c.name = n := by
      intro n hn'
      have : n ∈ names tb := List.mem_cons_of_mem _ hn'
      rw [hcols, ← colDefs_names] at *
      obtain ⟨c, hc, rfl⟩ := List.mem_map.1 this; exact ⟨c, hc, rfl⟩
    -- Once the statement is known: its rows are the tenant's rows passing the filter.
    have key : ∀ conds, (∀ c ∈ conds, CondOk kind tab.cols ps c) →
        (∀ x ∈ tab.rows, (AllHold kind tab.cols ps x conds ↔ x.head? = some tv ∧ Filter f x.tail)) →
        q = .select tb.name (tb.cols.map (·.name)) conds ((tb.cols.take tb.kl).map (·.name)) →
        ∃ R0, selected kind db q ps = some R0 ∧ ∀ R, R.Perm R0 → Sel (klOf ts) (view ts tv db) t (Filter f) R := by
      intro conds hok hall hq
      subst hq
      refine ⟨_, by simp only [selected, hdb]; rw [if_pos ⟨hok, hns⟩], fun R hR => ?_⟩
      have e : (tab.rows.filter (fun x => AllHold kind tab.cols ps x conds)).map
          (project tab.cols (tb.cols.map (·.name))) =
          ((tab.rows.filter (fun r => r.head? = some tv)).map List.tail).filter (Filter f) := by
        rw [List.filter_map, List.filter_filter]
        rw [List.map_congr_left (g := List.tail) (fun x hx => by
          rw [hcols]; exact project_cols tb hn x (hrowlen x (List.mem_filter.1 hx).1))]
        congr 1
        apply List.filter_congr
        intro x hx
        rw [hall x hx]
        simp only [Function.comp]
        by_cases h1 : x.head? = some tv <;> by_cases h2 : Filter f x.tail <;> simp [h1, h2]
      rw [e] at hR
      have hLn : ((tab.rows.filter (fun r => r.head? = some tv)).map List.tail).Nodup := hwk.1.of_map _
      refine ⟨hR.nodup_iff.2 (hLn.sublist List.filter_sublist), fun row => ?_⟩
      rw [hR.mem_iff, List.mem_filter, decide_eq_true_eq]
      simp only [view, readBack, klOf_some ht, hL]
      rw [Store.find_key_iff (fun r : List V => r.take tb.kl) _ hwk.1 _ row]
      simp
    cases f with
    | none =>
      simp only [Option.some.injEq, Prod.mk.injEq] at hc
      obtain ⟨rfl, rfl⟩ := hc
      apply key [Cond.eq tenantName 1]
      · intro c hc'; simp at hc'; subst hc'; rw [hcols]; exact condOk_tenant kind hn htv []
      · intro x _
        simp only [AllHold, List.mem_singleton, forall_eq, hcols, holds_tenant kind hn htv [] x, Filter, and_true]
      · rfl
    | some p =>
      obtain ⟨i, v⟩ := p
      cases hci : tb.cols[i]? with
      | none => simp [hci] at hc
      | some c =>
        simp only [hci] at hc
        split at hc
        · rename_i hty
          simp only [Option.some.injEq, Prod.mk.injEq] at hc
          obtain ⟨rfl, rfl⟩ := hc
          obtain ⟨hic, rfl⟩ := List.getElem?_eq_some_iff.1 hci
          have hcol := colIdx_col hn i hic
          apply key [Cond.eq tenantName 1, Cond.same tb.cols[i].name 2]
          · intro d hd
            rw [hcols]
            simp only [List.mem_cons, List.not_mem_nil, or_false] at hd
            rcases hd with rfl | rfl
            · exact condOk_tenant kind hn htv [v]
            · exact ⟨i + 1, _, v, hcol, colDefs_succ tb i hic, by simp [arg], hty⟩
          · intro x _
            have e : AllHold kind (colDefs tb) [tv, v] x [Cond.eq tenantName 1, Cond.same tb.cols[i].name 2] ↔
                Holds kind (colDefs tb) [tv, v] x (Cond.eq tenantName 1) ∧
                  Holds kind (colDefs tb) [tv, v] x (Cond.same tb.cols[i].name 2) := by
              simp [AllHold]
            rw [hcols, e, holds_tenant kind hn htv [v] x]
            simp only [Holds, hcol, Filter, ColIs, List.getElem?_tail]
            constructor
            · rintro ⟨hh, j, w, hj, hw, hr⟩
              cases hj; simp [arg] at hw; subst hw; exact ⟨hh, hr⟩
            · rintro ⟨hh, hr⟩
              exact ⟨hh, i + 1, v, rfl, by simp [arg], hr⟩
          · rfl
        · simp at hc

/-- A `SELECT` of every table lists the tenant's database: `Lists`. -/
theorem lists_of_selects {ts : List Tab} {tv : V} {db : PgDb V} (hv : Valid ts) (htv : kind tv = some .int)
    (hm : Matches kind ts db) (R : Tables V) (hlen : ∀ t, ts.length ≤ t → R t = [])
    (hsel : ∀ t, t < ts.length → ∃ q ps R0, selectA kind ts tv t none = some (q, ps) ∧
      selected kind db q ps = some R0 ∧ (R t).Perm R0) :
    Lists (klOf ts) (view ts tv db) R := by
  intro t
  by_cases ht : t < ts.length
  · obtain ⟨q, ps, R0, hq, hs, hp⟩ := hsel t ht
    obtain ⟨R1, hs1, hR⟩ := select_sound kind hv htv hm hq
    rw [hs] at hs1; cases hs1
    obtain ⟨hn, hmem⟩ := hR (R t) hp
    exact ⟨hn, fun row => by rw [hmem row]; simp [Filter]⟩
  · rw [hlen t (by omega)]
    refine ⟨List.nodup_nil, fun row => ?_⟩
    simp [view, readBack, tenantTabs, List.getElem?_eq_none (show ts.length ≤ t by omega)]

/-! ## Creating the tables -/

theorem keyNames_ok (tb : Tab) (hn : (names tb).Nodup) (hkl : tb.kl ≤ tb.cols.length) :
    (keyNames tb).Nodup ∧ ∀ n ∈ keyNames tb, ∃ c ∈ colDefs tb, c.name = n ∧ c.notNull = true := by
  rw [keyNames_take]
  refine ⟨hn.sublist (List.take_sublist _ _), fun n hn' => ?_⟩
  obtain ⟨j, hj, rfl⟩ := List.mem_iff_getElem.1 hn'
  simp only [List.length_take, names_length] at hj
  have hj' : j < (colDefs tb).length := by rw [colDefs_length]; omega
  refine ⟨(colDefs tb)[j], List.getElem_mem hj', ?_, ?_⟩
  · simp only [List.getElem_take, ← colDefs_names, List.getElem_map]
  · cases j with
    | zero => rfl
    | succ i => simp [colDefs]; omega

/-- Creating the tables in a database without them: each is then empty and as
`createA` says; nothing else changes. -/
theorem create_sound (l : List Tab) (hok : ∀ tb ∈ l, TabOk tb) (hn : (l.map (·.name)).Nodup) :
    ∀ db : PgDb V, (∀ tb ∈ l, db tb.name = none) →
      ∃ db', runAll kind db (l.map (fun tb => (createA tb, []))) = some db' ∧
        (∀ tb ∈ l, db' tb.name = some ⟨colDefs tb, keyNames tb, []⟩) ∧ ∀ n, n ∉ l.map (·.name) → db' n = db n := by
  induction l with
  | nil => intro db _; exact ⟨db, rfl, by simp, fun _ _ => rfl⟩
  | cons tb l ih =>
    intro db hnone
    simp only [List.map_cons, List.nodup_cons, List.mem_map] at hn
    have tok := hok tb List.mem_cons_self
    have hc : run kind db (createA tb) [] = some (put db tb.name ⟨colDefs tb, keyNames tb, []⟩) := by
      simp only [createA, run, hnone tb List.mem_cons_self]
      rw [if_pos ⟨by rw [colDefs_names]; exact tok.2.2.2.2.2.2, keyNames_ok tb tok.2.2.2.2.2.2 tok.2.2.2.2.1⟩]
    obtain ⟨db', hr, hin, hout⟩ := ih (fun tb' h => hok tb' (List.mem_cons_of_mem _ h)) hn.2
      (put db tb.name ⟨colDefs tb, keyNames tb, []⟩) (fun tb' h => by
      rw [put_other]
      · exact hnone tb' (List.mem_cons_of_mem _ h)
      · exact fun he => hn.1 ⟨tb', h, he⟩)
    refine ⟨db', by simp [runAll, hc, hr], fun tb' h => ?_, fun n hn' => ?_⟩
    · rcases List.mem_cons.1 h with rfl | h
      · rw [hout _ (fun hm => hn.1 (by simpa using hm)), put_self]
      · exact hin tb' h
    · simp only [List.map_cons, List.mem_cons, not_or] at hn'
      rw [hout n hn'.2, put_other _ _ _ _ hn'.1]

/-- After creating a valid schema's tables in a fresh database, it matches the
schema and every tenant's database is empty. -/
theorem create_fresh {ts : List Tab} (hv : Valid ts) (db : PgDb V) (hnone : ∀ tb ∈ ts, db tb.name = none) :
    ∃ db', runAll kind db (ts.map (fun tb => (createA tb, []))) = some db' ∧ Matches kind ts db' ∧
      (∀ tv, view ts tv db' = fun _ _ => none) ∧ ∀ n, n ∉ ts.map (·.name) → db' n = db n := by
  obtain ⟨db', hr, hin, hout⟩ := create_sound kind ts hv.1 hv.2 db hnone
  refine ⟨db', hr, fun tb h => ⟨_, hin tb h, rfl, rfl, by simp, by simp⟩, fun tv => ?_, hout⟩
  funext t k
  cases ht : ts[t]? with
  | none => simp [view, readBack, tenantTabs, ht]
  | some tb => simp [view, readBack, tenantTabs, ht, hin tb (List.mem_of_getElem? ht)]

/-- `CREATE TABLE IF NOT EXISTS` leaves existing tables as they are. -/
theorem create_kept (l : List Tab) :
    ∀ db : PgDb V, (∀ tb ∈ l, db tb.name ≠ none) → runAll kind db (l.map (fun tb => (createA tb, []))) = some db := by
  induction l with
  | nil => intro db _; rfl
  | cons tb l ih =>
    intro db h
    obtain ⟨tab, htab⟩ := Option.ne_none_iff_exists'.1 (h tb List.mem_cons_self)
    simp only [List.map_cons, runAll, createA, run, htab, Option.bind_some]
    exact ih db (fun tb' h' => h tb' (List.mem_cons_of_mem _ h'))

end Sound

end H5iAppLib.Pg
