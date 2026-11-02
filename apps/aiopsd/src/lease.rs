//! The orchestrator lease: a row in the control DB that names the one aiopsd instance allowed to
//! advance runs.
//!
//! The holder renews the lease at the start of every tick and from a heartbeat, so a long tick
//! (for example, several model turns) keeps it. On shutdown the holder stops renewing and
//! releases the lease, so the next instance can take over at once. If a holder dies without
//! releasing, the lease expires after `ttl_ms`.

use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::json;

use crate::db::Db;

pub struct Lease {
    db: Db,
    name: &'static str,
    holder: String,
    ttl_ms: u64,
    stopping: AtomicBool,
    held: AtomicBool,
    // Serialises renew and release, so a renewal that started before `stop` cannot take the
    // lease back after it was released.
    lock: tokio::sync::Mutex<()>,
}

impl Lease {
    pub fn new(db: Db, name: &'static str, holder: String, ttl_ms: u64) -> Self {
        Self {
            db,
            name,
            holder,
            ttl_ms,
            stopping: AtomicBool::new(false),
            held: AtomicBool::new(false),
            lock: tokio::sync::Mutex::new(()),
        }
    }

    pub fn holder(&self) -> &str {
        &self.holder
    }

    pub fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    /// Takes or extends the lease. Returns whether this instance holds it now.
    pub async fn renew(&self) -> anyhow::Result<bool> {
        self.renew_at(crate::now_ms()).await
    }

    async fn renew_at(&self, now: u64) -> anyhow::Result<bool> {
        let _guard = self.lock.lock().await;
        if self.is_stopping() {
            return Ok(false);
        }
        let changes = self
            .db
            .execute(
                "INSERT INTO leases (name, holder, expires_at) VALUES (?1, ?2, ?3) \
                 ON CONFLICT(name) DO UPDATE SET holder = excluded.holder, expires_at = excluded.expires_at \
                 WHERE leases.holder = excluded.holder OR leases.expires_at < ?4",
                vec![json!(self.name), json!(self.holder), json!(now + self.ttl_ms), json!(now)],
            )
            .await?;
        let held = changes == 1;
        if self.held.swap(held, Ordering::SeqCst) != held {
            if held {
                tracing::info!(lease = self.name, holder = %self.holder, "took the lease");
            } else {
                tracing::info!(lease = self.name, holder = %self.holder, "another instance holds the lease");
            }
        }
        Ok(held)
    }

    /// Stops renewing and gives the lease up. Call after the last tick has finished.
    pub async fn stop_and_release(&self) -> anyhow::Result<()> {
        let _guard = self.lock.lock().await;
        self.stopping.store(true, Ordering::SeqCst);
        self.held.store(false, Ordering::SeqCst);
        self.db
            .execute(
                "UPDATE leases SET expires_at = 0 WHERE name = ?1 AND holder = ?2",
                vec![json!(self.name), json!(self.holder)],
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    async fn db() -> Db {
        let migrations = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations");
        Db::connect("sqlite::memory:", &migrations).await.unwrap()
    }

    #[tokio::test]
    async fn one_holder_at_a_time() {
        let db = db().await;
        let a = Lease::new(db.clone(), "orchestrator", "a".into(), 60_000);
        let b = Lease::new(db.clone(), "orchestrator", "b".into(), 60_000);
        assert!(a.renew_at(1_000).await.unwrap());
        assert!(
            !b.renew_at(1_000).await.unwrap(),
            "b must wait while a holds it"
        );
        assert!(a.renew_at(30_000).await.unwrap(), "the holder can renew");
        assert!(
            !b.renew_at(60_000).await.unwrap(),
            "renewal pushed the expiry out"
        );
    }

    #[tokio::test]
    async fn expired_lease_can_be_taken() {
        let db = db().await;
        let a = Lease::new(db.clone(), "orchestrator", "a".into(), 60_000);
        let b = Lease::new(db.clone(), "orchestrator", "b".into(), 60_000);
        assert!(a.renew_at(1_000).await.unwrap());
        assert!(b.renew_at(61_001).await.unwrap(), "a stopped renewing");
        assert!(!a.renew_at(61_002).await.unwrap(), "a lost it to b");
    }

    #[tokio::test]
    async fn release_hands_over_at_once_and_stops_renewal() {
        let db = db().await;
        let a = Lease::new(db.clone(), "orchestrator", "a".into(), 60_000);
        let b = Lease::new(db.clone(), "orchestrator", "b".into(), 60_000);
        assert!(a.renew_at(1_000).await.unwrap());
        a.stop_and_release().await.unwrap();
        assert!(
            !a.renew_at(1_001).await.unwrap(),
            "a stopping instance never renews"
        );
        assert!(
            b.renew_at(1_002).await.unwrap(),
            "b takes over without waiting for expiry"
        );
    }
}
