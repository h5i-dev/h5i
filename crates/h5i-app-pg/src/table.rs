//! Kernel row structs as tenant-scoped tables.
//!
//! One struct is one table keyed by `(tenant_id, <key fields>)`. All SQL text
//! comes from `h5i_app_pgsql` (extracted to Lean); this file builds none.

use crate::DbError;
use bytes::BytesMut;
use h5i_app_core::TenantId;
use h5i_app_pgsql::{Query, Table as Spec};
use h5i_app_sql::{Val, Write as SqlWrite};
use tokio_postgres::types::{to_sql_checked, IsNull, ToSql, Type};
use crate::Tx;
use tokio_postgres::Row;

pub use h5i_app_pgsql::Kind;

/// A column value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Int(i64),
    Bool(bool),
    Text(String),
    Bytes(Vec<u8>),
    Null,
}

impl ToSql for Value {
    fn to_sql(
        &self,
        ty: &Type,
        out: &mut BytesMut,
    ) -> Result<IsNull, Box<dyn std::error::Error + Sync + Send>> {
        match self {
            Value::Int(v) => v.to_sql(ty, out),
            Value::Bool(v) => v.to_sql(ty, out),
            Value::Text(v) => v.to_sql(ty, out),
            Value::Bytes(v) => v.to_sql(ty, out),
            Value::Null => Ok(IsNull::Yes),
        }
    }

    fn accepts(ty: &Type) -> bool {
        matches!(*ty, Type::INT8 | Type::BOOL | Type::TEXT | Type::BYTEA)
    }

    to_sql_checked!();
}

#[derive(Clone, Debug)]
pub struct ColumnDef {
    pub name: &'static str,
    pub kind: Kind,
    pub nullable: bool,
}

/// How a field is stored. `A` is the app marker type, to dodge the orphan rule.
pub trait PgField<A>: Sized {
    const KIND: Kind;
    const NULLABLE: bool = false;
    fn to_value(&self) -> Result<Value, DbError>;
    fn from_value(v: &Value) -> Result<Self, DbError>;
}

fn bad(what: &str, v: &Value) -> DbError {
    DbError::Decode(format!("expected {what}, found {v:?}"))
}

macro_rules! int_field {
    ($($t:ty),*) => {$(
        impl<A> PgField<A> for $t {
            const KIND: Kind = Kind::Int;
            fn to_value(&self) -> Result<Value, DbError> {
                i64::try_from(*self)
                    .map(Value::Int)
                    .map_err(|_| DbError::Decode(format!("{} does not fit in BIGINT", self)))
            }
            fn from_value(v: &Value) -> Result<Self, DbError> {
                match v {
                    Value::Int(i) => <$t>::try_from(*i).map_err(|_| bad(stringify!($t), v)),
                    _ => Err(bad(stringify!($t), v)),
                }
            }
        }
    )*};
}
int_field!(u8, u16, u32, i64);

// Bits kept, as `h5i_app_sql::Column` does: above `i64::MAX` is stored negative.
impl<A> PgField<A> for u64 {
    const KIND: Kind = Kind::Int;
    fn to_value(&self) -> Result<Value, DbError> {
        Ok(Value::Int(*self as i64))
    }
    fn from_value(v: &Value) -> Result<Self, DbError> {
        match v {
            Value::Int(i) => Ok(*i as u64),
            _ => Err(bad("u64", v)),
        }
    }
}

impl<A> PgField<A> for bool {
    const KIND: Kind = Kind::Bool;
    fn to_value(&self) -> Result<Value, DbError> {
        Ok(Value::Bool(*self))
    }
    fn from_value(v: &Value) -> Result<Self, DbError> {
        match v {
            Value::Bool(b) => Ok(*b),
            _ => Err(bad("bool", v)),
        }
    }
}

impl<A> PgField<A> for String {
    const KIND: Kind = Kind::Text;
    fn to_value(&self) -> Result<Value, DbError> {
        Ok(Value::Text(self.clone()))
    }
    fn from_value(v: &Value) -> Result<Self, DbError> {
        match v {
            Value::Text(s) => Ok(s.clone()),
            _ => Err(bad("text", v)),
        }
    }
}

impl<A> PgField<A> for Vec<u8> {
    const KIND: Kind = Kind::Bytes;
    fn to_value(&self) -> Result<Value, DbError> {
        Ok(Value::Bytes(self.clone()))
    }
    fn from_value(v: &Value) -> Result<Self, DbError> {
        match v {
            Value::Bytes(b) => Ok(b.clone()),
            _ => Err(bad("bytes", v)),
        }
    }
}

impl<A, T: PgField<A>> PgField<A> for Option<T> {
    const KIND: Kind = T::KIND;
    const NULLABLE: bool = true;
    fn to_value(&self) -> Result<Value, DbError> {
        match self {
            Some(t) => t.to_value(),
            None => Ok(Value::Null),
        }
    }
    fn from_value(v: &Value) -> Result<Self, DbError> {
        match v {
            Value::Null => Ok(None),
            _ => T::from_value(v).map(Some),
        }
    }
}

/// Column type inferred from a field projection. Used by `table!`.
pub fn column_of<A, R, T: PgField<A>>(name: &'static str, _field: fn(&R) -> &T) -> ColumnDef {
    ColumnDef { name, kind: T::KIND, nullable: T::NULLABLE }
}

/// Implement with [`table!`](crate::table).
pub trait Table<A>: Sized + Send + Sync {
    const NAME: &'static str;
    /// Leading columns that form the key, after `tenant_id`.
    const KEY_LEN: usize;
    fn columns() -> Vec<ColumnDef>;
    fn to_values(&self) -> Result<Vec<Value>, DbError>;
    fn from_values(v: &[Value]) -> Result<Self, DbError>;
}

/// Declares how a kernel row struct maps to a table.
///
/// ```ignore
/// h5i_app_pg::table!(DocsApp, kernel::Member => "members" { key: [project, user], cols: [role] });
/// ```
///
/// Every field must be listed, or the generated constructor does not compile.
#[macro_export]
macro_rules! table {
    ($app:ty, $row:path => $name:literal { key: [$($k:ident),*], cols: [$($c:ident),* $(,)?] $(,)? }) => {
        impl $crate::Table<$app> for $row {
            const NAME: &'static str = $name;
            const KEY_LEN: usize = 0 $(+ { let _ = stringify!($k); 1 })*;
            fn columns() -> Vec<$crate::ColumnDef> {
                vec![
                    $($crate::column_of::<$app, Self, _>(stringify!($k), |r: &Self| &r.$k),)*
                    $($crate::column_of::<$app, Self, _>(stringify!($c), |r: &Self| &r.$c),)*
                ]
            }
            fn to_values(&self) -> Result<Vec<$crate::Value>, $crate::DbError> {
                Ok(vec![
                    $(<_ as $crate::PgField<$app>>::to_value(&self.$k)?,)*
                    $(<_ as $crate::PgField<$app>>::to_value(&self.$c)?,)*
                ])
            }
            fn from_values(v: &[$crate::Value]) -> Result<Self, $crate::DbError> {
                let mut it = v.iter();
                let mut next = || it.next().ok_or_else(|| $crate::DbError::Decode("short row".into()));
                Ok(Self {
                    $($k: <_ as $crate::PgField<$app>>::from_value(next()?)?,)*
                    $($c: <_ as $crate::PgField<$app>>::from_value(next()?)?,)*
                })
            }
        }
    };
}

/// The table `T` maps to, as the SQL compiler sees it.
pub fn spec<A, T: Table<A>>() -> Spec {
    let columns = T::columns()
        .into_iter()
        .map(|c| h5i_app_pgsql::Column { name: c.name.as_bytes().to_vec(), kind: c.kind, nullable: c.nullable })
        .collect();
    Spec { name: T::NAME.as_bytes().to_vec(), columns, key_len: T::KEY_LEN as u32 }
}

fn invalid(schema: &[Spec]) -> DbError {
    let names: Vec<_> = schema.iter().map(|t| String::from_utf8_lossy(&t.name).into_owned()).collect();
    DbError::Decode(format!(
        "invalid table schema [{}]: names must be 1 to 63 bytes without NUL, distinct, not `tenant_id` \
         for a column nor `h5i_...` for a table, with 1 to 1599 columns",
        names.join(", ")
    ))
}

/// `CREATE TABLE IF NOT EXISTS` for every table of `schema`, in order.
pub fn create_tables(schema: &Vec<Spec>) -> Result<Vec<String>, DbError> {
    if !h5i_app_pgsql::valid(schema) {
        return Err(invalid(schema));
    }
    schema.iter().map(|t| text(&h5i_app_pgsql::create(t))).collect()
}

fn text(sql: &h5i_app_pgsql::Sql) -> Result<String, DbError> {
    String::from_utf8(h5i_app_pgsql::render(sql)).map_err(|e| DbError::Decode(e.to_string()))
}

/// `q`'s rendered text and its parameters as driver values.
fn bind(q: &Query) -> Result<(String, Vec<Value>), DbError> {
    Ok((text(&q.sql)?, q.params.iter().map(from_val).collect::<Result<Vec<_>, _>>()?))
}

fn refs(vals: &[Value]) -> Vec<&(dyn ToSql + Sync)> {
    vals.iter().map(|v| v as &(dyn ToSql + Sync)).collect()
}

/// Plan (`h5i_app_sql::plan`), compile (`h5i_app_pgsql::compile`) and run each write in
/// order. A statement that does not fit the schema fails the transaction.
pub async fn store_writes(tx: &Tx<'_>, tenant: TenantId, schema: &Vec<Spec>, ws: &Vec<SqlWrite>) -> Result<(), DbError> {
    let tid = tenant_param(tenant)?;
    for stmt in h5i_app_sql::plan(ws) {
        let Some(q) = h5i_app_pgsql::compile(schema, tid, &stmt) else {
            return Err(DbError::Decode(format!("statement does not fit the schema: {stmt:?}")));
        };
        let (sql, vals) = bind(&q)?;
        tx.0.execute(&sql, &refs(&vals)).await?;
    }
    Ok(())
}

/// The tenant's rows of table `table`, without `tenant_id`, in key order.
/// `filter` keeps rows whose zero-based column IS NOT DISTINCT FROM the value,
/// matching `H5iAppLib.Sql.ColIs`.
pub async fn load_table(
    tx: &Tx<'_>,
    tenant: TenantId,
    schema: &Vec<Spec>,
    table: u32,
    filter: Option<(u32, Val)>,
) -> Result<Vec<Vec<Val>>, DbError> {
    let Some(q) = h5i_app_pgsql::select(schema, tenant_param(tenant)?, table, filter) else {
        return Err(DbError::Decode(format!("no table {table} or column to filter on, or an invalid schema")));
    };
    let (sql, vals) = bind(&q)?;
    let kinds: Vec<Kind> = schema[table as usize].columns.iter().map(|c| c.kind).collect();
    let rows = tx.0.query(&sql, &refs(&vals)).await?;
    rows.iter().map(|r| read_row(&kinds, r)).collect()
}

fn read_row(kinds: &[Kind], row: &Row) -> Result<Vec<Val>, DbError> {
    let mut out = Vec::with_capacity(kinds.len());
    for (i, k) in kinds.iter().enumerate() {
        let v = match k {
            Kind::Int => row.try_get::<_, Option<i64>>(i)?.map(Value::Int),
            Kind::Bool => row.try_get::<_, Option<bool>>(i)?.map(Value::Bool),
            Kind::Text => row.try_get::<_, Option<String>>(i)?.map(Value::Text),
            Kind::Bytes => row.try_get::<_, Option<Vec<u8>>>(i)?.map(Value::Bytes),
        };
        out.push(to_val(&v.unwrap_or(Value::Null)));
    }
    Ok(out)
}

fn to_val(v: &Value) -> Val {
    match v {
        Value::Int(i) => Val::Int(*i),
        Value::Bool(b) => Val::Bool(*b),
        Value::Text(s) => Val::Text(s.clone().into_bytes()),
        Value::Bytes(b) => Val::Bytes(b.clone()),
        Value::Null => Val::Null,
    }
}

fn from_val(v: &Val) -> Result<Value, DbError> {
    Ok(match v {
        Val::Int(i) => Value::Int(*i),
        Val::Bool(b) => Value::Bool(*b),
        Val::Text(b) => Value::Text(String::from_utf8(b.clone()).map_err(|e| DbError::Decode(e.to_string()))?),
        Val::Bytes(b) => Value::Bytes(b.clone()),
        Val::Null => Value::Null,
    })
}

pub(crate) fn tenant_param(t: TenantId) -> Result<i64, DbError> {
    i64::try_from(t.0).map_err(|_| DbError::Decode(format!("tenant id {} out of range", t.0)))
}
