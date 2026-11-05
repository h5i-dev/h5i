//! A connection whose request was cancelled (its future dropped) while it
//! may still have a statement running on the server: waiting for an
//! advisory lock, for a row lock, or for a deferrable snapshot.
//!
//! Such a connection stays counted against its pool until it is known to be
//! clean, so cancelled requests can never hold more server backends than the
//! pool's size: a cleanup task cancels whatever runs on it
//! (`CancelToken::cancel_query`), rolls back, releases every advisory lock,
//! and only then returns it to the pool. If that does not succeed within
//! [`DEADLINE`], the connection is detached from the pool, but at most
//! [`MAX_DETACHED`] per pool at once; beyond that it stays in the pool's
//! count and the cleanup keeps trying. Without a tokio runtime (a drop
//! during shutdown) it is detached at once and closes with its socket.

use crate::DbError;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use tokio_postgres::error::SqlState;
use tokio_postgres::{CancelToken, NoTls};

/// One try of cancel, `ROLLBACK`, `pg_advisory_unlock_all()`.
const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(5);
/// After this long, detach the connection if a slot allows.
const DEADLINE: Duration = Duration::from_secs(30);
/// Detached connections a pool may have whose sockets are not yet closed.
pub(crate) const MAX_DETACHED: usize = 2;

/// How to reach the server for a cancel request: as the pool connects.
#[derive(Clone)]
pub(crate) enum Connector {
    Plain,
    #[cfg(feature = "tls")]
    Tls(crate::tls::RustlsConnect),
}

/// What the cleanups of one pool did.
#[derive(Debug, Default)]
pub struct CleanupStats {
    /// Connections cleaned and returned to the pool.
    pub cleaned: AtomicU64,
    /// Connections detached from the pool (no runtime, or past the deadline).
    pub detached: AtomicU64,
}

pub(crate) struct Cleanup {
    connector: Connector,
    detached: Arc<Semaphore>,
    pub(crate) stats: CleanupStats,
}

impl Cleanup {
    pub(crate) fn new(connector: Connector) -> Arc<Self> {
        Arc::new(Cleanup { connector, detached: Arc::new(Semaphore::new(MAX_DETACHED)), stats: CleanupStats::default() })
    }

    async fn cancel(&self, t: CancelToken) -> Result<(), tokio_postgres::Error> {
        match &self.connector {
            Connector::Plain => t.cancel_query(NoTls).await,
            #[cfg(feature = "tls")]
            Connector::Tls(c) => t.cancel_query(c.clone()).await,
        }
    }

    /// Cancel what runs, then end any transaction and release every
    /// advisory lock. The cancel may land late, on one of these statements:
    /// then they run again.
    async fn reset(&self, c: &deadpool_postgres::Object) -> Result<(), DbError> {
        self.cancel(c.cancel_token()).await?;
        let mut last = None;
        for _ in 0..3 {
            let r = async {
                c.batch_execute("ROLLBACK").await?;
                c.execute("SELECT pg_advisory_unlock_all()", &[]).await?;
                Ok::<_, tokio_postgres::Error>(())
            }
            .await;
            match r {
                Ok(()) => return Ok(()),
                Err(e) if e.code() == Some(&SqlState::QUERY_CANCELED) => last = Some(e),
                Err(e) => return Err(e.into()),
            }
        }
        Err(last.map(DbError::Postgres).unwrap_or_else(|| DbError::Pool("cleanup".into())))
    }

    /// Clean `obj` and return it to the pool, or detach it (see the module docs).
    async fn clean(self: Arc<Self>, obj: deadpool_postgres::Object) {
        // Dropped mid-cleanup (the runtime shuts down): detach.
        let mut held = Detached(Some(obj));
        let start = tokio::time::Instant::now();
        loop {
            let c = held.0.as_ref().expect("held until returned");
            if c.is_closed() {
                // Nothing runs on a closed connection; the pool discards it.
                held.0.take();
                return;
            }
            if let Ok(Ok(())) = tokio::time::timeout(ATTEMPT_TIMEOUT, self.reset(c)).await {
                self.stats.cleaned.fetch_add(1, Ordering::Relaxed);
                held.0.take();
                return;
            }
            if start.elapsed() >= DEADLINE
                && let Ok(permit) = self.detached.clone().try_acquire_owned()
            {
                self.stats.detached.fetch_add(1, Ordering::Relaxed);
                let client = deadpool_postgres::Object::take(held.0.take().expect("held"));
                // The slot is free again once the socket is closed.
                while !client.is_closed() {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                drop(permit);
                return;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}

struct Detached(Option<deadpool_postgres::Object>);

impl Drop for Detached {
    fn drop(&mut self) {
        if let Some(obj) = self.0.take() {
            drop(deadpool_postgres::Object::take(obj));
        }
    }
}

/// An attempt's connection. Returned to the pool by [`Conn::release`] once
/// the attempt left it clean (no statement running, no advisory lock held);
/// dropped any other way, it goes through [`Cleanup`].
pub(crate) struct Conn {
    obj: Option<deadpool_postgres::Object>,
    cleanup: Arc<Cleanup>,
}

impl Conn {
    pub(crate) fn new(obj: deadpool_postgres::Object, cleanup: Arc<Cleanup>) -> Self {
        Conn { obj: Some(obj), cleanup }
    }

    pub(crate) fn get(&mut self) -> &mut deadpool_postgres::Object {
        self.obj.as_mut().expect("connection present until released")
    }

    /// Back to the pool: nothing runs on it and it holds no advisory lock.
    pub(crate) fn release(&mut self) {
        self.obj.take();
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        let Some(obj) = self.obj.take() else { return };
        match tokio::runtime::Handle::try_current() {
            Ok(rt) => {
                rt.spawn(self.cleanup.clone().clean(obj));
            }
            Err(_) => {
                self.cleanup.stats.detached.fetch_add(1, Ordering::Relaxed);
                drop(deadpool_postgres::Object::take(obj));
            }
        }
    }
}
