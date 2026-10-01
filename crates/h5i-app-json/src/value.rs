//! A JSON tree for building replies. Not extracted; `tokens` is checked
//! against serde_json by tests.

use crate::Tok;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Null,
    Bool(bool),
    Num(u64),
    Str(Vec<u8>),
    Arr(Vec<Value>),
    Obj(Vec<(Vec<u8>, Value)>),
}

impl Value {
    pub fn str(s: impl AsRef<[u8]>) -> Value {
        Value::Str(s.as_ref().to_vec())
    }

    /// Object from `(key, value)` pairs, in order.
    pub fn obj<K: AsRef<[u8]>>(fields: impl IntoIterator<Item = (K, Value)>) -> Value {
        Value::Obj(fields.into_iter().map(|(k, v)| (k.as_ref().to_vec(), v)).collect())
    }

    pub fn tokens(&self) -> Vec<Tok> {
        let mut out = Vec::new();
        self.push_tokens(&mut out);
        out
    }

    fn push_tokens(&self, out: &mut Vec<Tok>) {
        match self {
            Value::Null => out.push(Tok::Null),
            Value::Bool(b) => out.push(Tok::Bool(*b)),
            Value::Num(n) => out.push(Tok::Num(*n)),
            Value::Str(s) => out.push(Tok::Str(s.clone())),
            Value::Arr(xs) => {
                out.push(Tok::ArrOpen);
                for x in xs {
                    x.push_tokens(out);
                }
                out.push(Tok::ArrClose);
            }
            Value::Obj(fs) => {
                out.push(Tok::ObjOpen);
                for (k, v) in fs {
                    out.push(Tok::Key(k.clone()));
                    v.push_tokens(out);
                }
                out.push(Tok::ObjClose);
            }
        }
    }

    /// JSON text of this value.
    pub fn to_bytes(&self) -> Vec<u8> {
        crate::write(&self.tokens())
    }
}

impl From<u64> for Value {
    fn from(n: u64) -> Value {
        Value::Num(n)
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Value {
        Value::Bool(b)
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Value {
        Value::str(s)
    }
}

impl<T: Into<Value>> From<Option<T>> for Value {
    fn from(o: Option<T>) -> Value {
        match o {
            Some(v) => v.into(),
            None => Value::Null,
        }
    }
}
