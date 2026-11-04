use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// Counts SQL statements and DB time for one request.
#[derive(Clone, Default)]
pub struct Tracker(Arc<Counters>);

#[derive(Default)]
struct Counters {
    sql_count: AtomicU64,
    db_ns: AtomicU64,
}

impl Tracker {
    /// Awaits one database call and records it.
    pub async fn run<F: Future>(&self, query: F) -> F::Output {
        let started = Instant::now();
        let out = query.await;
        self.0.sql_count.fetch_add(1, Ordering::Relaxed);
        self.0.db_ns.fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);
        out
    }

    pub fn sql_count(&self) -> u64 {
        self.0.sql_count.load(Ordering::Relaxed)
    }

    pub fn db_ms(&self) -> f64 {
        self.0.db_ns.load(Ordering::Relaxed) as f64 / 1_000_000.0
    }
}
