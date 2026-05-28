//! Background cache refreshers for `mode: refresh` and `mode: cron`.
//!
//! Each refresher is a detached Tokio task that periodically calls
//! [`CacheManager::refresh`](super::CacheManager::refresh) for one table. The
//! engine keeps the [`JoinHandle`]s and aborts them on shutdown.

use std::str::FromStr;
use std::sync::Arc;

use chrono::Utc;
use cron::Schedule;
use datafusion::execution::context::SessionContext;
use pawrly_core::{CachePolicy, TableName};
use tokio::runtime::Handle;
use tokio::task::JoinHandle;
use tokio::time::{self, Duration, MissedTickBehavior};

use super::CacheManager;

/// Spawns background refresh loops for the modes that have one.
pub(crate) struct Spawner;

impl Spawner {
    /// Spawn a refresher for `key` if `policy` is `refresh` or `cron`.
    /// Returns `None` for modes without a background loop, or when the policy is
    /// malformed (zero interval, unparseable cron).
    pub(crate) fn spawn_for(
        handle: &Handle,
        key: TableName,
        policy: CachePolicy,
        manager: Arc<CacheManager>,
        ctx: SessionContext,
    ) -> Option<JoinHandle<()>> {
        match policy {
            CachePolicy::Refresh { every } => {
                if every.is_zero() {
                    tracing::warn!(table = %key, "refresh interval is zero; refresher not started");
                    return None;
                }
                Some(handle.spawn(interval_loop(every, key, manager, ctx)))
            }
            CachePolicy::Cron { cron } => match Schedule::from_str(&cron) {
                Ok(schedule) => Some(handle.spawn(cron_loop(schedule, key, manager, ctx))),
                Err(e) => {
                    tracing::warn!(error = %e, table = %key, "invalid cron expression; refresher not started");
                    None
                }
            },
            _ => None,
        }
    }
}

async fn interval_loop(
    every: Duration,
    key: TableName,
    manager: Arc<CacheManager>,
    ctx: SessionContext,
) {
    let mut ticker = time::interval(every);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    // The first tick fires immediately; the initial population happens on the
    // first read, so skip it and wait a full interval before refreshing.
    ticker.tick().await;
    loop {
        ticker.tick().await;
        run_once(&key, &manager, &ctx).await;
    }
}

async fn cron_loop(
    schedule: Schedule,
    key: TableName,
    manager: Arc<CacheManager>,
    ctx: SessionContext,
) {
    loop {
        let Some(next) = schedule.upcoming(Utc).next() else {
            break;
        };
        let wait = (next - Utc::now())
            .to_std()
            .unwrap_or(Duration::from_secs(0));
        time::sleep(wait).await;
        run_once(&key, &manager, &ctx).await;
    }
}

async fn run_once(key: &TableName, manager: &Arc<CacheManager>, ctx: &SessionContext) {
    match manager.refresh(key, ctx).await {
        Ok(out) => {
            tracing::info!(table = %key, rows = out.rows_written, "cache refresh succeeded")
        }
        Err(e) => {
            tracing::warn!(error = %e, table = %key, "cache refresh failed; will retry")
        }
    }
}
