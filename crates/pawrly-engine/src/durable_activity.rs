//! Durable on-disk backing for `system.activity`.
//!
//! Records are buffered, then flushed in batches to date/hour-partitioned
//! Parquet files (`dt=YYYY-MM-DD/hr=HH/<time>-<uuid>.parquet`); a JSON manifest
//! tracks each file's time range and row count so reads and retention can prune
//! whole files without opening them. Flushing clears the buffer, so files and
//! the pending buffer never overlap — a scan reads `files ∪ pending` with no
//! deduplication. Writes are atomic (tmp, fsync, rename) and the manifest is
//! guarded by a cross-process advisory lock.

use std::fs::File;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use arrow_array::RecordBatch;
use arrow_schema::SchemaRef;
use async_trait::async_trait;
use chrono::{Timelike as _, Utc};
use fs2::FileExt as _;
use parking_lot::Mutex;
use parquet::arrow::ArrowWriter;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use pawrly_core::EngineError;
use pawrly_core::activity::{ActivityRecord, ActivityRecorder};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::system_table::{activity_schema, records_to_batch};

const MANIFEST: &str = "manifest.json";
const LOCK: &str = ".manifest.lock";

/// On-disk index of the Parquet files making up the durable store.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Manifest {
    files: Vec<FileEntry>,
}

/// One Parquet file's metadata. `min_at`/`max_at` are microseconds since the
/// epoch, enabling read/retention pruning without opening the file.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FileEntry {
    /// Path relative to the store directory.
    path: String,
    min_at: i64,
    max_at: i64,
    rows: u64,
}

/// Durable backing for `system.activity`. Cloneable handle over shared state, so
/// the recorder (write) and table provider (read) observe the same store.
#[derive(Clone)]
pub struct DurableActivityStore {
    inner: Arc<Inner>,
}

struct Inner {
    dir: PathBuf,
    /// `hr=` bucket width in hours, clamped to `1..=24`.
    partition_hours: u32,
    /// Flush once the pending buffer reaches this many records.
    flush_threshold: usize,
    /// Drop files older than this; `None` keeps all history.
    retention: Option<Duration>,
    /// Unflushed records, the most recent tail not yet on disk.
    pending: Mutex<Vec<ActivityRecord>>,
}

impl DurableActivityStore {
    /// Open (creating if needed) a durable store at `dir`. A non-zero
    /// `flush_interval` spawns a background task that flushes the pending buffer
    /// (and applies retention) on that cadence so a quiet daemon still persists
    /// promptly. Retention is also applied once at open.
    pub fn open(
        dir: PathBuf,
        partition_hours: u32,
        flush_threshold: usize,
        flush_interval: Duration,
        retention: Option<Duration>,
    ) -> Result<Self, EngineError> {
        std::fs::create_dir_all(&dir).map_err(|e| {
            EngineError::Internal(format!("activity store create {}: {e}", dir.display()))
        })?;
        let store = Self {
            inner: Arc::new(Inner {
                dir,
                partition_hours: partition_hours.clamp(1, 24),
                flush_threshold: flush_threshold.max(1),
                retention,
                pending: Mutex::new(Vec::new()),
            }),
        };
        store.prune_if_due();
        if !flush_interval.is_zero() {
            store.spawn_flush_timer(flush_interval);
        }
        Ok(store)
    }

    fn spawn_flush_timer(&self, interval: Duration) {
        let weak = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(interval);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                let Some(inner) = weak.upgrade() else {
                    break; // store dropped; stop the timer
                };
                let store = DurableActivityStore { inner };
                if let Err(e) = store.flush() {
                    tracing::warn!(error = %e, "activity: timed durable flush failed");
                }
                store.prune_if_due();
            }
        });
    }

    /// Apply the configured retention, logging on failure.
    fn prune_if_due(&self) {
        if let Some(retention) = self.inner.retention
            && let Err(e) = self.prune(retention)
        {
            tracing::warn!(error = %e, "activity: retention prune failed");
        }
    }

    /// Write the pending buffer to a new Parquet file and record it in the
    /// manifest. A no-op when there is nothing pending.
    pub fn flush(&self) -> Result<(), EngineError> {
        let records = {
            let mut pending = self.inner.pending.lock();
            std::mem::take(&mut *pending)
        };
        if records.is_empty() {
            return Ok(());
        }

        let schema = activity_schema();
        let batch = records_to_batch(&records, &schema)
            .map_err(|e| EngineError::Internal(format!("activity batch: {e}")))?;
        let (min_at, max_at) = records.iter().map(|r| r.at.timestamp_micros()).fold(
            (i64::MAX, i64::MIN),
            |(lo, hi), v| (lo.min(v), hi.max(v)),
        );

        let (rel, abs) = self.next_file_path();
        if let Some(parent) = abs.parent() {
            io(std::fs::create_dir_all(parent), "create partition dir")?;
        }
        write_parquet(&self.inner.dir, &abs, &schema, &batch)?;

        self.with_manifest(|m| {
            m.files.push(FileEntry {
                path: rel,
                min_at,
                max_at,
                rows: records.len() as u64,
            });
        })
    }

    /// All on-disk batches plus the current pending buffer. The two never
    /// overlap, so no deduplication is needed.
    pub fn read_batches(&self, schema: &SchemaRef) -> Result<Vec<RecordBatch>, EngineError> {
        let manifest = read_manifest(&self.inner.dir);
        let mut batches = Vec::new();
        for entry in &manifest.files {
            let path = self.inner.dir.join(&entry.path);
            match read_parquet(&path) {
                Ok(mut b) => batches.append(&mut b),
                // A missing/corrupt file is skipped rather than failing the
                // whole query; the rest of the history is still returned.
                Err(e) => tracing::warn!(error = %e, path = %path.display(), "activity: skipping unreadable file"),
            }
        }
        let pending = self.inner.pending.lock().clone();
        if !pending.is_empty() {
            batches.push(
                records_to_batch(&pending, schema)
                    .map_err(|e| EngineError::Internal(format!("activity batch: {e}")))?,
            );
        }
        Ok(batches)
    }

    /// Delete files whose newest record is older than `retention`, then drop
    /// their manifest entries and any now-empty partition directories.
    pub fn prune(&self, retention: Duration) -> Result<(), EngineError> {
        let cutoff = Utc::now().timestamp_micros()
            - i64::try_from(retention.as_micros()).unwrap_or(i64::MAX);
        self.with_manifest(|m| {
            m.files.retain(|entry| {
                if entry.max_at >= cutoff {
                    return true;
                }
                let path = self.inner.dir.join(&entry.path);
                let _ = std::fs::remove_file(&path);
                if let Some(parent) = path.parent() {
                    let _ = std::fs::remove_dir(parent); // only if now empty
                    if let Some(day) = parent.parent() {
                        let _ = std::fs::remove_dir(day);
                    }
                }
                false
            });
        })
    }

    /// `dt=YYYY-MM-DD/hr=HH/<HHMMSS>-<uuid>.parquet` for the current flush,
    /// bucketed by wall-clock time. Returns `(relative, absolute)`.
    fn next_file_path(&self) -> (String, PathBuf) {
        let now = Utc::now();
        let bucket = (now.hour() / self.inner.partition_hours) * self.inner.partition_hours;
        let rel = format!(
            "dt={}/hr={:02}/{}-{}.parquet",
            now.format("%Y-%m-%d"),
            bucket,
            now.format("%H%M%S"),
            Uuid::new_v4(),
        );
        let abs = self.inner.dir.join(&rel);
        (rel, abs)
    }

    /// Load-modify-write the manifest under a cross-process advisory lock.
    fn with_manifest(&self, f: impl FnOnce(&mut Manifest)) -> Result<(), EngineError> {
        let lock = io(File::create(self.inner.dir.join(LOCK)), "open manifest lock")?;
        io(lock.lock_exclusive(), "lock manifest")?;
        let mut manifest = read_manifest(&self.inner.dir);
        f(&mut manifest);
        let result = write_manifest(&self.inner.dir, &manifest);
        let _ = fs2::FileExt::unlock(&lock);
        result
    }
}

#[async_trait]
impl ActivityRecorder for DurableActivityStore {
    async fn record(&self, rec: ActivityRecord) {
        let full = {
            let mut pending = self.inner.pending.lock();
            pending.push(rec);
            pending.len() >= self.inner.flush_threshold
        };
        if full && let Err(e) = self.flush() {
            tracing::warn!(error = %e, "activity: durable flush failed");
        }
    }
}

fn read_manifest(dir: &Path) -> Manifest {
    std::fs::read(dir.join(MANIFEST))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn write_manifest(dir: &Path, manifest: &Manifest) -> Result<(), EngineError> {
    let bytes = serde_json::to_vec_pretty(manifest)
        .map_err(|e| EngineError::Internal(format!("serialize activity manifest: {e}")))?;
    let tmp = dir.join(format!("{MANIFEST}.tmp.{}", Uuid::new_v4()));
    {
        let mut file = io(File::create(&tmp), "create manifest tmp")?;
        io(file.write_all(&bytes), "write manifest")?;
        io(file.sync_all(), "fsync manifest")?;
    }
    io(std::fs::rename(&tmp, dir.join(MANIFEST)), "rename manifest")
}

/// Atomic Parquet write: tmp file, fsync, rename, then fsync the parent dir.
fn write_parquet(
    dir: &Path,
    final_path: &Path,
    schema: &SchemaRef,
    batch: &RecordBatch,
) -> Result<(), EngineError> {
    let tmp = dir.join(format!(".{}.parquet.tmp", Uuid::new_v4()));
    {
        let file = io(File::create(&tmp), "create parquet tmp")?;
        let mut writer = ArrowWriter::try_new(file, schema.clone(), None)
            .map_err(|e| EngineError::Internal(format!("parquet writer: {e}")))?;
        writer
            .write(batch)
            .map_err(|e| EngineError::Internal(format!("parquet write: {e}")))?;
        let file = writer
            .into_inner()
            .map_err(|e| EngineError::Internal(format!("parquet close: {e}")))?;
        io(file.sync_all(), "fsync parquet")?;
    }
    io(std::fs::rename(&tmp, final_path), "rename parquet")?;
    if let Some(parent) = final_path.parent()
        && let Ok(dir) = File::open(parent)
    {
        let _ = dir.sync_all();
    }
    Ok(())
}

fn read_parquet(path: &Path) -> Result<Vec<RecordBatch>, EngineError> {
    let file = io(File::open(path), "open parquet")?;
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)
        .map_err(|e| EngineError::Internal(format!("parquet open: {e}")))?
        .build()
        .map_err(|e| EngineError::Internal(format!("parquet read: {e}")))?;
    let mut batches = Vec::new();
    for batch in reader {
        batches.push(batch.map_err(|e| EngineError::Internal(format!("parquet batch: {e}")))?);
    }
    Ok(batches)
}

/// Map an `io::Error` to an `EngineError` with a context label.
fn io<T>(result: std::io::Result<T>, what: &str) -> Result<T, EngineError> {
    result.map_err(|e| EngineError::Internal(format!("activity store {what}: {e}")))
}
