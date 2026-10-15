//! A bound on each connection's prepared statements.
//!
//! deadpool keeps every statement a connection ever prepared. `Statements`
//! remembers, per connection, the order its cached statements were last
//! used, and once a connection holds more than the capacity it removes the
//! least recently used one from deadpool's cache. Dropping the last handle
//! of a tokio-postgres `Statement` closes it on the server, so server-side
//! prepared statements stay bounded too (a statement still in use by a
//! pipelined query is closed when that query is done with it).

use deadpool_postgres::StatementCache;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};

/// Default number of prepared statements kept per connection.
pub const DEFAULT_CAPACITY: usize = 256;

struct Conn {
    /// Keeps the cache's allocation, so its address is never another
    /// connection's while this entry exists.
    cache: Weak<StatementCache>,
    /// Least recently used first.
    order: VecDeque<String>,
}

pub(crate) struct Statements {
    capacity: AtomicUsize,
    conns: Mutex<HashMap<usize, Conn>>,
}

impl Statements {
    pub(crate) fn new(capacity: usize) -> Self {
        Statements { capacity: AtomicUsize::new(capacity), conns: Mutex::new(HashMap::new()) }
    }

    pub(crate) fn set_capacity(&self, capacity: usize) {
        self.capacity.store(capacity, Ordering::Relaxed);
    }

    pub(crate) fn capacity(&self) -> usize {
        self.capacity.load(Ordering::Relaxed)
    }

    /// `sql` was just prepared or reused on the connection owning `cache`.
    /// Evicts least recently used statements beyond the capacity.
    pub(crate) fn touch(&self, cache: &Arc<StatementCache>, sql: &str) {
        let cap = self.capacity();
        let mut evicted = Vec::new();
        {
            let mut conns = self.conns.lock().unwrap_or_else(|e| e.into_inner());
            let key = Arc::as_ptr(cache) as usize;
            if !conns.contains_key(&key) {
                // A new connection: forget the ones that are gone.
                conns.retain(|_, c| c.cache.strong_count() > 0);
                conns.insert(key, Conn { cache: Arc::downgrade(cache), order: VecDeque::new() });
            }
            let conn = conns.get_mut(&key).expect("just inserted");
            if let Some(i) = conn.order.iter().position(|s| s == sql) {
                let s = conn.order.remove(i).expect("position is in range");
                conn.order.push_back(s);
            } else {
                conn.order.push_back(sql.to_string());
            }
            while conn.order.len() > cap {
                if let Some(old) = conn.order.pop_front() {
                    evicted.push(old);
                }
            }
        }
        // Outside the lock: dropping a statement sends a Close to the server.
        for old in evicted {
            drop(cache.remove(&old, &[]));
        }
    }

    /// Statements tracked for the connection owning `cache`.
    #[cfg(test)]
    pub(crate) fn tracked(&self, cache: &Arc<StatementCache>) -> usize {
        let conns = self.conns.lock().unwrap_or_else(|e| e.into_inner());
        conns.get(&(Arc::as_ptr(cache) as usize)).map_or(0, |c| c.order.len())
    }
}
