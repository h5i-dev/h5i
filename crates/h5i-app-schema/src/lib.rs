//! One declaration for a kernel's snapshot and row types.
//!
//! ```ignore
//! h5i_app_schema::schema! {
//!     mapping board_tables for board_kernel, writes Write, lean "../proofs/generated/Schema.lean";
//!
//!     #[derive(Clone, Debug, Default, PartialEq, Eq)]
//!     pub struct Snapshot {
//!         counter: Counter,
//!         posts: Vec<Post>,
//!     }
//!
//!     #[derive(Clone, Debug, PartialEq, Eq)]
//!     pub struct Post in "posts" {
//!         key { id: u64 }
//!         author: u64,
//!         text: Text,
//!     }
//!
//!     #[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
//!     pub struct Counter in "counters" {
//!         key {}
//!         next_id: u64,
//!     }
//! }
//! ```
//!
//! Row structs stay plain Rust for Aeneas. Each gets `TABLE`, `KEY_LEN`,
//! `to_row`/`from_row` and `sql_put`; a keyed row also gets `put`, `del`,
//! `del_where` and their `sql_*` writes. A row without key fields is a
//! singleton. The snapshot gets `Rows` and `decode`, and with `writes Write`,
//! `apply` and `sql_writes` over the kernel's `apply_write` and `sql_write`.
//!
//! The mapping name is a module of column indices (`board_tables::Post::author`)
//! and a macro for the server crate, `board_kernel::board_tables!(Board)`,
//! which defines the `h5i_app_pg` tables and the `schema_*` load and store
//! functions. `lean "path"` keeps the generated Lean schema current; a
//! generated test checks it and `H5I_APP_BLESS=1` rewrites it.

#[doc(hidden)]
pub mod lean;

#[macro_export]
macro_rules! schema {
    (
        mapping $mapping:ident for $krate:ident $(, writes $w:ident)? $(, lean $lean:literal)?;
        $(#[$sattr:meta])*
        $svis:vis struct $snap:ident { $($sfields:tt)* }
        $($rows:tt)*
    ) => {
        $crate::__snapshot! {
            @parse [$mapping $krate [$($w)?] [$($lean)?]] [$(#[$sattr])*] [$svis] $snap [] [$($sfields)*] [$($rows)*]
        }
    };
    (
        mapping $mapping:ident for $krate:ident $(, writes $w:ident)? $(, lean $lean:literal)?;
        $($rows:tt)*
    ) => {
        $crate::__schema! { [$mapping $krate [$($w)?] [$($lean)?]] [] $($rows)* }
    };
}

/// Parses snapshot fields (`Vec<Row>` or `Row`) into `[name Row]` pairs.
#[doc(hidden)]
#[macro_export]
macro_rules! __snapshot {
    (@parse $hdr:tt $sattr:tt $svis:tt $snap:ident [$($acc:tt)*] [] [$($rows:tt)*]) => {
        $crate::__schema! { $hdr [$sattr $svis $snap [$($acc)*]] $($rows)* }
    };
    (@parse $hdr:tt $sattr:tt $svis:tt $snap:ident [$($acc:tt)*]
        [$(#[$fa:meta])* $fv:vis $f:ident : Vec<$t:ident> $(, $($more:tt)*)?] $rows:tt) => {
        $crate::__snapshot! { @parse $hdr $sattr $svis $snap [$($acc)* [[$(#[$fa])*] $f $t [Vec<$t>]]] [$($($more)*)?] $rows }
    };
    (@parse $hdr:tt $sattr:tt $svis:tt $snap:ident [$($acc:tt)*]
        [$(#[$fa:meta])* $fv:vis $f:ident : $t:ident $(, $($more:tt)*)?] $rows:tt) => {
        $crate::__snapshot! { @parse $hdr $sattr $svis $snap [$($acc)* [[$(#[$fa])*] $f $t [$t]]] [$($($more)*)?] $rows }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __schema {
    (
        [$mapping:ident $krate:ident [$($w:ident)?] [$($lean:literal)?]] $snap:tt
        $(
            $(#[$attr:meta])*
            $vis:vis struct $name:ident in $table:literal {
                key { $($(#[$kattr:meta])* $k:ident : $kt:ty),* $(,)? }
                $($(#[$cattr:meta])* $c:ident : $ct:ty),* $(,)?
            }
        )*
    ) => {
        $(
            $(#[$attr])*
            $vis struct $name {
                $($(#[$kattr])* pub $k: $kt,)*
                $($(#[$cattr])* pub $c: $ct,)*
            }
        )*
        /// Column indices in `to_row` order, shared with the Lean schema and PostgreSQL mappings.
        #[allow(non_snake_case, non_upper_case_globals)]
        pub mod $mapping {
            $(
                pub mod $name {
                    $crate::__column_indices! { [] $($k)* $($c)* }
                }
            )*
        }
        $crate::__rows! { [] $( $name [$($k)*] [$($c)*] )* }
        $( $crate::__keyed! { $name [$($k : $kt),*] [$($c)*] } )*
        $crate::__snap_items! { [$($w)?] $snap }
        $crate::__lean! { [$($lean)?] $krate [$($w)?] $snap; $( $name $table [$($k : $kt),*] [$($c : $ct),*] )* }
        $crate::__mapping! { ($) $mapping $krate [$($w)?] $snap; $( $name $table [$($k)*] [$($c)*] )* }
    };
}

/// One constant per column; the accumulator keeps each value a literal for Aeneas.
#[doc(hidden)]
#[macro_export]
macro_rules! __column_indices {
    ([$($i:tt)*]) => {};
    ([$($i:tt)*] $field:ident $($rest:ident)*) => {
        pub const $field: u32 = $crate::__count!($($i)*);
        $crate::__column_indices! { [$($i)* $field] $($rest)* }
    };
}

/// Table number, key length and SQL encoding per row type. Plain Rust for Aeneas.
#[doc(hidden)]
#[macro_export]
macro_rules! __rows {
    ([$($i:tt)*]) => {};
    ([$($i:tt)*] $name:ident [$($k:ident)*] [$($c:ident)*] $($rest:tt)*) => {
        impl $name {
            pub const TABLE: u32 = $crate::__count!($($i)*);
            pub const KEY_LEN: u32 = $crate::__count!($($k)*);

            pub fn to_row(&self) -> Vec<::h5i_app_sql::Val> {
                let mut out = Vec::new();
                $(out.push(::h5i_app_sql::Column::to_val(&self.$k));)*
                $(out.push(::h5i_app_sql::Column::to_val(&self.$c));)*
                out
            }

            /// Inverse of `to_row`; `None` if the row has the wrong shape.
            pub fn from_row(row: &Vec<::h5i_app_sql::Val>) -> Option<Self> {
                if row.len() != 0 $(+ $crate::__one!($k))* $(+ $crate::__one!($c))* {
                    return None;
                }
                let mut i: usize = 0;
                $(
                    let $k = match ::h5i_app_sql::Column::from_val(&row[i]) {
                        Some(x) => x,
                        None => return None,
                    };
                    i += 1;
                )*
                $(
                    let $c = match ::h5i_app_sql::Column::from_val(&row[i]) {
                        Some(x) => x,
                        None => return None,
                    };
                    i += 1;
                )*
                let _ = i;
                Some($name { $($k,)* $($c,)* })
            }

            /// Every row decoded, in order; `None` if any row fails.
            pub fn from_rows(rows: &Vec<Vec<::h5i_app_sql::Val>>) -> Option<Vec<Self>> {
                let mut out = Vec::new();
                let mut ok = true;
                let mut i = 0;
                while i < rows.len() {
                    match Self::from_row(&rows[i]) {
                        Some(x) => out.push(x),
                        None => ok = false,
                    }
                    i += 1;
                }
                if ok {
                    Some(out)
                } else {
                    None
                }
            }

            /// The table write that stores this row.
            pub fn sql_put(&self) -> ::h5i_app_sql::Write {
                ::h5i_app_sql::Write::Put { table: Self::TABLE, key_len: Self::KEY_LEN, row: self.to_row() }
            }
        }
        $crate::__rows! { [$($i)* $name] $($rest)* }
    };
}

/// Table operations for keyed rows; `from_one` for singletons.
#[doc(hidden)]
#[macro_export]
macro_rules! __keyed {
    ($name:ident [] [$($c:ident)*]) => {
        impl $name {
            /// The table's row; a fresh tenant has none, which reads as zeros.
            pub fn from_one(rows: &Vec<Vec<::h5i_app_sql::Val>>) -> Option<Self> {
                if rows.len() == 0 {
                    Some($name { $($c: ::h5i_app_sql::Zero::zero(),)* })
                } else if rows.len() == 1 {
                    Self::from_row(&rows[0])
                } else {
                    None
                }
            }
        }
    };
    ($name:ident [$($k:ident : $kt:ty),+] [$($c:ident)*]) => {
        impl $name {
            /// Insert `x`, or replace the row with its key.
            pub fn put(v: &mut Vec<Self>, x: Self) {
                let mut i = 0;
                while i < v.len() {
                    if $(v[i].$k == x.$k)&&+ {
                        v[i] = x;
                        return;
                    }
                    i += 1;
                }
                v.push(x);
            }

            /// The rows without this key.
            pub fn del(v: &Vec<Self>, $($k: $kt),+) -> Vec<Self> {
                let mut out = Vec::new();
                let mut i = 0;
                while i < v.len() {
                    if !($(v[i].$k == $k)&&+) {
                        out.push(v[i].clone());
                    }
                    i += 1;
                }
                out
            }

            /// The rows whose column `col` (in `to_row` order) does not hold `val`.
            pub fn del_where(v: &Vec<Self>, col: u32, val: &::h5i_app_sql::Val) -> Vec<Self> {
                let mut out = Vec::new();
                let mut i = 0;
                while i < v.len() {
                    if !::h5i_app_sql::has_col(&v[i].to_row(), col, val) {
                        out.push(v[i].clone());
                    }
                    i += 1;
                }
                out
            }

            /// The table write that deletes the row with this key.
            pub fn sql_del($($k: $kt),+) -> ::h5i_app_sql::Write {
                let mut key = Vec::new();
                $(key.push(::h5i_app_sql::Column::to_val(&$k));)+
                ::h5i_app_sql::Write::Del { table: Self::TABLE, key }
            }

            /// The table write that deletes the rows whose column `col` holds `val`.
            pub fn sql_del_where(col: u32, val: ::h5i_app_sql::Val) -> ::h5i_app_sql::Write {
                ::h5i_app_sql::Write::DelWhere { table: Self::TABLE, col, val }
            }
        }
    };
}

/// The snapshot struct, `Rows`, `decode`, and with writes `apply`/`sql_writes`.
#[doc(hidden)]
#[macro_export]
macro_rules! __snap_items {
    ([$($w:ident)?] []) => {};
    ([] [[$(#[$sattr:meta])*] [$svis:vis] $snap:ident [$([[$(#[$fa:meta])*] $f:ident $t:ident [$($ty:tt)*]])*]]) => {
        $(#[$sattr])*
        $svis struct $snap {
            $($(#[$fa])* pub $f: $($ty)*,)*
        }

        /// A tenant's stored rows per table, in database order.
        #[derive(Clone, Debug, Default, PartialEq, Eq)]
        pub struct Rows {
            $(pub $f: Vec<Vec<::h5i_app_sql::Val>>,)*
        }

        /// The snapshot stored rows stand for; `None` if a row does not decode.
        pub fn decode(r: &Rows) -> Option<$snap> {
            $(
                let $f = match $crate::__from_table!($t [$($ty)*] &r.$f) {
                    Some(x) => x,
                    None => return None,
                };
            )*
            Some($snap { $($f,)* })
        }
    };
    ([$w:ident] [[$(#[$sattr:meta])*] [$svis:vis] $snap:ident $fields:tt]) => {
        $crate::__snap_items! { [] [[$(#[$sattr])*] [$svis] $snap $fields] }

        /// Commit semantics: `apply_write` on each write in order. The PostgreSQL store must agree.
        pub fn apply(snap: &$snap, ws: &Vec<$w>) -> $snap {
            let mut s = snap.clone();
            let mut i = 0;
            while i < ws.len() {
                apply_write(&mut s, ws[i].clone());
                i += 1;
            }
            s
        }

        /// `sql_write` on each write in order. The server runs exactly these.
        pub fn sql_writes(ws: &Vec<$w>) -> Vec<::h5i_app_sql::Write> {
            let mut out = Vec::new();
            let mut i = 0;
            while i < ws.len() {
                sql_write(&ws[i], &mut out);
                i += 1;
            }
            out
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __from_table {
    ($t:ident [Vec<$t2:ident>] $e:expr) => {
        $t::from_rows($e)
    };
    ($t:ident [$t2:ident] $e:expr) => {
        $t::from_one($e)
    };
}

/// With `lean "path"`: the Lean renderer and a test that `path` is current.
#[doc(hidden)]
#[macro_export]
macro_rules! __lean {
    ([] $($rest:tt)*) => {};
    ([$lean:literal] $krate:ident [$($w:ident)?] $snap:tt; $( $name:ident $table:literal [$($k:ident : $kt:ty),*] [$($c:ident : $ct:ty),*] )*) => {
        #[doc(hidden)]
        pub fn __h5i_app_lean_schema() -> ::std::string::String {
            let rows: ::std::vec::Vec<$crate::lean::RowDecl> = ::std::vec![
                $( (
                    stringify!($name),
                    $table,
                    ::std::vec![
                        $((stringify!($k), ::std::any::type_name::<$kt>()),)*
                        $((stringify!($c), ::std::any::type_name::<$ct>()),)*
                    ],
                    0 $(+ $crate::__one!($k))*,
                ), )*
            ];
            let snap: ::std::option::Option<(&str, ::std::vec::Vec<(&str, &str)>)> = $crate::__lean_snap!($snap);
            let writes: ::std::option::Option<&str> = $crate::__lean_opt!($($w)?);
            $crate::lean::render(stringify!($krate), &rows, snap.as_ref(), writes)
        }

        #[cfg(test)]
        #[test]
        fn h5i_app_lean_schema_is_current() {
            $crate::lean::check(concat!(env!("CARGO_MANIFEST_DIR"), "/", $lean), &__h5i_app_lean_schema());
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __lean_snap {
    ([]) => {
        ::std::option::Option::None
    };
    ([$sattr:tt $svis:tt $snap:ident [$([$fa:tt $f:ident $t:ident $ty:tt])*]]) => {
        ::std::option::Option::Some((stringify!($snap), ::std::vec![$((stringify!($f), stringify!($t)),)*]))
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __lean_opt {
    () => {
        ::std::option::Option::None
    };
    ($w:ident) => {
        ::std::option::Option::Some(stringify!($w))
    };
}

/// The number of tokens, as a literal, so Aeneas sees a plain constant.
#[doc(hidden)]
#[macro_export]
#[rustfmt::skip]
macro_rules! __count {
    () => { 0 };
    ($x0:tt) => { 1 };
    ($x0:tt $x1:tt) => { 2 };
    ($x0:tt $x1:tt $x2:tt) => { 3 };
    ($x0:tt $x1:tt $x2:tt $x3:tt) => { 4 };
    ($x0:tt $x1:tt $x2:tt $x3:tt $x4:tt) => { 5 };
    ($x0:tt $x1:tt $x2:tt $x3:tt $x4:tt $x5:tt) => { 6 };
    ($x0:tt $x1:tt $x2:tt $x3:tt $x4:tt $x5:tt $x6:tt) => { 7 };
    ($x0:tt $x1:tt $x2:tt $x3:tt $x4:tt $x5:tt $x6:tt $x7:tt) => { 8 };
    ($x0:tt $x1:tt $x2:tt $x3:tt $x4:tt $x5:tt $x6:tt $x7:tt $x8:tt) => { 9 };
    ($x0:tt $x1:tt $x2:tt $x3:tt $x4:tt $x5:tt $x6:tt $x7:tt $x8:tt $x9:tt) => { 10 };
    ($x0:tt $x1:tt $x2:tt $x3:tt $x4:tt $x5:tt $x6:tt $x7:tt $x8:tt $x9:tt $x10:tt) => { 11 };
    ($x0:tt $x1:tt $x2:tt $x3:tt $x4:tt $x5:tt $x6:tt $x7:tt $x8:tt $x9:tt $x10:tt $x11:tt) => { 12 };
    ($x0:tt $x1:tt $x2:tt $x3:tt $x4:tt $x5:tt $x6:tt $x7:tt $x8:tt $x9:tt $x10:tt $x11:tt $x12:tt) => { 13 };
    ($x0:tt $x1:tt $x2:tt $x3:tt $x4:tt $x5:tt $x6:tt $x7:tt $x8:tt $x9:tt $x10:tt $x11:tt $x12:tt $x13:tt) => { 14 };
    ($x0:tt $x1:tt $x2:tt $x3:tt $x4:tt $x5:tt $x6:tt $x7:tt $x8:tt $x9:tt $x10:tt $x11:tt $x12:tt $x13:tt $x14:tt) => { 15 };
    ($x0:tt $x1:tt $x2:tt $x3:tt $x4:tt $x5:tt $x6:tt $x7:tt $x8:tt $x9:tt $x10:tt $x11:tt $x12:tt $x13:tt $x14:tt $x15:tt) => { 16 };
    ($x0:tt $x1:tt $x2:tt $x3:tt $x4:tt $x5:tt $x6:tt $x7:tt $x8:tt $x9:tt $x10:tt $x11:tt $x12:tt $x13:tt $x14:tt $x15:tt $x16:tt) => { 17 };
    ($x0:tt $x1:tt $x2:tt $x3:tt $x4:tt $x5:tt $x6:tt $x7:tt $x8:tt $x9:tt $x10:tt $x11:tt $x12:tt $x13:tt $x14:tt $x15:tt $x16:tt $x17:tt) => { 18 };
    ($x0:tt $x1:tt $x2:tt $x3:tt $x4:tt $x5:tt $x6:tt $x7:tt $x8:tt $x9:tt $x10:tt $x11:tt $x12:tt $x13:tt $x14:tt $x15:tt $x16:tt $x17:tt $x18:tt) => { 19 };
    ($x0:tt $x1:tt $x2:tt $x3:tt $x4:tt $x5:tt $x6:tt $x7:tt $x8:tt $x9:tt $x10:tt $x11:tt $x12:tt $x13:tt $x14:tt $x15:tt $x16:tt $x17:tt $x18:tt $x19:tt) => { 20 };
    ($x0:tt $x1:tt $x2:tt $x3:tt $x4:tt $x5:tt $x6:tt $x7:tt $x8:tt $x9:tt $x10:tt $x11:tt $x12:tt $x13:tt $x14:tt $x15:tt $x16:tt $x17:tt $x18:tt $x19:tt $x20:tt) => { 21 };
    ($x0:tt $x1:tt $x2:tt $x3:tt $x4:tt $x5:tt $x6:tt $x7:tt $x8:tt $x9:tt $x10:tt $x11:tt $x12:tt $x13:tt $x14:tt $x15:tt $x16:tt $x17:tt $x18:tt $x19:tt $x20:tt $x21:tt) => { 22 };
    ($x0:tt $x1:tt $x2:tt $x3:tt $x4:tt $x5:tt $x6:tt $x7:tt $x8:tt $x9:tt $x10:tt $x11:tt $x12:tt $x13:tt $x14:tt $x15:tt $x16:tt $x17:tt $x18:tt $x19:tt $x20:tt $x21:tt $x22:tt) => { 23 };
    ($x0:tt $x1:tt $x2:tt $x3:tt $x4:tt $x5:tt $x6:tt $x7:tt $x8:tt $x9:tt $x10:tt $x11:tt $x12:tt $x13:tt $x14:tt $x15:tt $x16:tt $x17:tt $x18:tt $x19:tt $x20:tt $x21:tt $x22:tt $x23:tt) => { 24 };
    ($x0:tt $x1:tt $x2:tt $x3:tt $x4:tt $x5:tt $x6:tt $x7:tt $x8:tt $x9:tt $x10:tt $x11:tt $x12:tt $x13:tt $x14:tt $x15:tt $x16:tt $x17:tt $x18:tt $x19:tt $x20:tt $x21:tt $x22:tt $x23:tt $x24:tt) => { 25 };
    ($x0:tt $x1:tt $x2:tt $x3:tt $x4:tt $x5:tt $x6:tt $x7:tt $x8:tt $x9:tt $x10:tt $x11:tt $x12:tt $x13:tt $x14:tt $x15:tt $x16:tt $x17:tt $x18:tt $x19:tt $x20:tt $x21:tt $x22:tt $x23:tt $x24:tt $x25:tt) => { 26 };
    ($x0:tt $x1:tt $x2:tt $x3:tt $x4:tt $x5:tt $x6:tt $x7:tt $x8:tt $x9:tt $x10:tt $x11:tt $x12:tt $x13:tt $x14:tt $x15:tt $x16:tt $x17:tt $x18:tt $x19:tt $x20:tt $x21:tt $x22:tt $x23:tt $x24:tt $x25:tt $x26:tt) => { 27 };
    ($x0:tt $x1:tt $x2:tt $x3:tt $x4:tt $x5:tt $x6:tt $x7:tt $x8:tt $x9:tt $x10:tt $x11:tt $x12:tt $x13:tt $x14:tt $x15:tt $x16:tt $x17:tt $x18:tt $x19:tt $x20:tt $x21:tt $x22:tt $x23:tt $x24:tt $x25:tt $x26:tt $x27:tt) => { 28 };
    ($x0:tt $x1:tt $x2:tt $x3:tt $x4:tt $x5:tt $x6:tt $x7:tt $x8:tt $x9:tt $x10:tt $x11:tt $x12:tt $x13:tt $x14:tt $x15:tt $x16:tt $x17:tt $x18:tt $x19:tt $x20:tt $x21:tt $x22:tt $x23:tt $x24:tt $x25:tt $x26:tt $x27:tt $x28:tt) => { 29 };
    ($x0:tt $x1:tt $x2:tt $x3:tt $x4:tt $x5:tt $x6:tt $x7:tt $x8:tt $x9:tt $x10:tt $x11:tt $x12:tt $x13:tt $x14:tt $x15:tt $x16:tt $x17:tt $x18:tt $x19:tt $x20:tt $x21:tt $x22:tt $x23:tt $x24:tt $x25:tt $x26:tt $x27:tt $x28:tt $x29:tt) => { 30 };
    ($x0:tt $x1:tt $x2:tt $x3:tt $x4:tt $x5:tt $x6:tt $x7:tt $x8:tt $x9:tt $x10:tt $x11:tt $x12:tt $x13:tt $x14:tt $x15:tt $x16:tt $x17:tt $x18:tt $x19:tt $x20:tt $x21:tt $x22:tt $x23:tt $x24:tt $x25:tt $x26:tt $x27:tt $x28:tt $x29:tt $x30:tt) => { 31 };
    ($x0:tt $x1:tt $x2:tt $x3:tt $x4:tt $x5:tt $x6:tt $x7:tt $x8:tt $x9:tt $x10:tt $x11:tt $x12:tt $x13:tt $x14:tt $x15:tt $x16:tt $x17:tt $x18:tt $x19:tt $x20:tt $x21:tt $x22:tt $x23:tt $x24:tt $x25:tt $x26:tt $x27:tt $x28:tt $x29:tt $x30:tt $x31:tt) => { 32 };
    ($x0:tt $x1:tt $x2:tt $x3:tt $x4:tt $x5:tt $x6:tt $x7:tt $x8:tt $x9:tt $x10:tt $x11:tt $x12:tt $x13:tt $x14:tt $x15:tt $x16:tt $x17:tt $x18:tt $x19:tt $x20:tt $x21:tt $x22:tt $x23:tt $x24:tt $x25:tt $x26:tt $x27:tt $x28:tt $x29:tt $x30:tt $x31:tt $x32:tt) => { 33 };
    ($x0:tt $x1:tt $x2:tt $x3:tt $x4:tt $x5:tt $x6:tt $x7:tt $x8:tt $x9:tt $x10:tt $x11:tt $x12:tt $x13:tt $x14:tt $x15:tt $x16:tt $x17:tt $x18:tt $x19:tt $x20:tt $x21:tt $x22:tt $x23:tt $x24:tt $x25:tt $x26:tt $x27:tt $x28:tt $x29:tt $x30:tt $x31:tt $x32:tt $x33:tt) => { 34 };
    ($x0:tt $x1:tt $x2:tt $x3:tt $x4:tt $x5:tt $x6:tt $x7:tt $x8:tt $x9:tt $x10:tt $x11:tt $x12:tt $x13:tt $x14:tt $x15:tt $x16:tt $x17:tt $x18:tt $x19:tt $x20:tt $x21:tt $x22:tt $x23:tt $x24:tt $x25:tt $x26:tt $x27:tt $x28:tt $x29:tt $x30:tt $x31:tt $x32:tt $x33:tt $x34:tt) => { 35 };
    ($x0:tt $x1:tt $x2:tt $x3:tt $x4:tt $x5:tt $x6:tt $x7:tt $x8:tt $x9:tt $x10:tt $x11:tt $x12:tt $x13:tt $x14:tt $x15:tt $x16:tt $x17:tt $x18:tt $x19:tt $x20:tt $x21:tt $x22:tt $x23:tt $x24:tt $x25:tt $x26:tt $x27:tt $x28:tt $x29:tt $x30:tt $x31:tt $x32:tt $x33:tt $x34:tt $x35:tt) => { 36 };
    ($x0:tt $x1:tt $x2:tt $x3:tt $x4:tt $x5:tt $x6:tt $x7:tt $x8:tt $x9:tt $x10:tt $x11:tt $x12:tt $x13:tt $x14:tt $x15:tt $x16:tt $x17:tt $x18:tt $x19:tt $x20:tt $x21:tt $x22:tt $x23:tt $x24:tt $x25:tt $x26:tt $x27:tt $x28:tt $x29:tt $x30:tt $x31:tt $x32:tt $x33:tt $x34:tt $x35:tt $x36:tt) => { 37 };
    ($x0:tt $x1:tt $x2:tt $x3:tt $x4:tt $x5:tt $x6:tt $x7:tt $x8:tt $x9:tt $x10:tt $x11:tt $x12:tt $x13:tt $x14:tt $x15:tt $x16:tt $x17:tt $x18:tt $x19:tt $x20:tt $x21:tt $x22:tt $x23:tt $x24:tt $x25:tt $x26:tt $x27:tt $x28:tt $x29:tt $x30:tt $x31:tt $x32:tt $x33:tt $x34:tt $x35:tt $x36:tt $x37:tt) => { 38 };
    ($x0:tt $x1:tt $x2:tt $x3:tt $x4:tt $x5:tt $x6:tt $x7:tt $x8:tt $x9:tt $x10:tt $x11:tt $x12:tt $x13:tt $x14:tt $x15:tt $x16:tt $x17:tt $x18:tt $x19:tt $x20:tt $x21:tt $x22:tt $x23:tt $x24:tt $x25:tt $x26:tt $x27:tt $x28:tt $x29:tt $x30:tt $x31:tt $x32:tt $x33:tt $x34:tt $x35:tt $x36:tt $x37:tt $x38:tt) => { 39 };
    ($x0:tt $x1:tt $x2:tt $x3:tt $x4:tt $x5:tt $x6:tt $x7:tt $x8:tt $x9:tt $x10:tt $x11:tt $x12:tt $x13:tt $x14:tt $x15:tt $x16:tt $x17:tt $x18:tt $x19:tt $x20:tt $x21:tt $x22:tt $x23:tt $x24:tt $x25:tt $x26:tt $x27:tt $x28:tt $x29:tt $x30:tt $x31:tt $x32:tt $x33:tt $x34:tt $x35:tt $x36:tt $x37:tt $x38:tt $x39:tt) => { 40 };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __one {
    ($x:ident) => {
        1
    };
}

/// Defines the mapping macro. `$` is passed in so it can have its own `$app`.
#[doc(hidden)]
#[macro_export]
macro_rules! __mapping {
    (($d:tt) $mapping:ident $krate:ident [$($w:ident)?] $snap:tt; $( $name:ident $table:literal [$($k:ident)*] [$($c:ident)*] )*) => {
        #[macro_export]
        macro_rules! $mapping {
            ($d app:ty) => {
                $( ::h5i_app_pg::table!($d app, $krate::$name => $table { key: [$($k),*], cols: [$($c),*] }); )*

                /// Every table, numbered like `TABLE`, for `h5i_app_pgsql`.
                #[allow(dead_code)]
                fn schema_spec() -> ::std::vec::Vec<::h5i_app_pg::pgsql::Table> {
                    ::std::vec![ $( ::h5i_app_pg::spec::<$d app, $krate::$name>() ),* ]
                }

                /// `CREATE TABLE` statements. Panics on an invalid schema; `schema_is_valid` catches that.
                fn schema_ddl() -> ::std::vec::Vec<::std::string::String> {
                    ::h5i_app_pg::create_tables(&schema_spec()).expect("schema! tables are valid")
                }

                #[cfg(test)]
                #[test]
                fn schema_is_valid() {
                    let spec = schema_spec();
                    assert!(::h5i_app_pg::pgsql::valid(&spec), "h5i_app_pgsql rejects this schema");
                    let mut n: u32 = 0;
                    $(
                        assert_eq!($krate::$name::TABLE, n);
                        assert_eq!(spec[n as usize].key_len, $krate::$name::KEY_LEN);
                        n += 1;
                    )*
                    let _ = n;
                }

                /// Plan, compile and run the kernel's table writes in order.
                #[allow(dead_code)]
                async fn schema_write(
                    tx: &::h5i_app_pg::Tx<'_>,
                    tenant: ::h5i_app_core::TenantId,
                    ws: &::std::vec::Vec<::h5i_app_pg::sql::Write>,
                ) -> ::std::result::Result<(), ::h5i_app_pg::DbError> {
                    ::h5i_app_pg::store_writes(tx, tenant, &schema_spec(), ws).await
                }

                /// The tenant's rows of `table` whose `column` is not distinct from `value`.
                #[allow(dead_code)]
                async fn schema_rows_where(
                    tx: &::h5i_app_pg::Tx<'_>,
                    tenant: ::h5i_app_core::TenantId,
                    table: u32,
                    column: u32,
                    value: &::h5i_app_pg::sql::Val,
                ) -> ::std::result::Result<::std::vec::Vec<::std::vec::Vec<::h5i_app_pg::sql::Val>>, ::h5i_app_pg::DbError> {
                    ::h5i_app_pg::load_table(tx, tenant, &schema_spec(), table, Some((column, value.clone()))).await
                }

                /// All of the tenant's rows of table `table`.
                #[allow(dead_code)]
                async fn schema_rows(
                    tx: &::h5i_app_pg::Tx<'_>,
                    tenant: ::h5i_app_core::TenantId,
                    table: u32,
                ) -> ::std::result::Result<::std::vec::Vec<::std::vec::Vec<::h5i_app_pg::sql::Val>>, ::h5i_app_pg::DbError> {
                    ::h5i_app_pg::load_table(tx, tenant, &schema_spec(), table, None).await
                }

                /// Table names, in `TABLE` order.
                fn schema_tables() -> ::std::vec::Vec<&'static str> {
                    ::std::vec![ $( <$krate::$name as ::h5i_app_pg::Table<$d app>>::NAME ),* ]
                }

                $crate::__mapping_snap! { [$d app] $krate [$($w)?] $snap }
            };
        }
    };
}

/// The server's load and store for a schema with a snapshot.
#[doc(hidden)]
#[macro_export]
macro_rules! __mapping_snap {
    ([$app:ty] $krate:ident [$($w:ident)?] []) => {};
    ([$app:ty] $krate:ident [$($w:ident)?] [$sattr:tt $svis:tt $snap:ident [$([$fa:tt $f:ident $t:ident $ty:tt])*]]) => {
        /// Load every table for the tenant and `decode` it.
        #[allow(dead_code)]
        async fn schema_load(
            tx: &::h5i_app_pg::Tx<'_>,
            tenant: ::h5i_app_core::TenantId,
        ) -> ::std::result::Result<$krate::$snap, ::h5i_app_pg::DbError> {
            let spec = schema_spec();
            let rows = $krate::Rows { $($f: ::h5i_app_pg::load_table(tx, tenant, &spec, $krate::$t::TABLE, None).await?,)* };
            $krate::decode(&rows).ok_or_else(|| ::h5i_app_pg::DbError::Decode("stored rows do not decode".into()))
        }

        $crate::__mapping_store! { $krate [$($w)?] }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __mapping_store {
    ($krate:ident []) => {};
    ($krate:ident [$w:ident]) => {
        /// Store a write set's `sql_writes`.
        #[allow(dead_code)]
        async fn schema_store(
            tx: &::h5i_app_pg::Tx<'_>,
            tenant: ::h5i_app_core::TenantId,
            ws: &::std::vec::Vec<$krate::$w>,
        ) -> ::std::result::Result<(), ::h5i_app_pg::DbError> {
            schema_write(tx, tenant, &$krate::sql_writes(ws)).await
        }
    };
}
