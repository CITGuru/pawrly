//! Cache layer: opt-in per-table caching to Parquet + a JSON manifest.
//!
//! Writes are atomic (write to `tmp/`, fsync, rename into place) and the
//! manifest is guarded by a cross-process advisory lock, so the cache survives
//! `kill -9` mid-write and concurrent CLI + daemon processes don't corrupt each
//! other. `ttl`, `refresh`, and `cron` modes are supported; the background
//! refreshers for the latter two live in [`refresher`].

use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use arrow_array::RecordBatch;
use arrow_schema::SchemaRef;
use async_trait::async_trait;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use datafusion::catalog::Session;
use datafusion::common::DataFusionError;
use datafusion::datasource::memory::MemorySourceConfig;
use datafusion::datasource::{TableProvider, TableType};
use datafusion::execution::context::SessionContext;
use datafusion::logical_expr::{Expr, TableProviderFilterPushDown};
use datafusion::physical_plan::{ExecutionPlan, collect};
use fs2::FileExt as _;
use parking_lot::Mutex;
use parquet::arrow::{ArrowWriter, arrow_reader::ParquetRecordBatchReaderBuilder};
use pawrly_core::{
    CacheEntryInfo, CacheMode, CachePolicy, EngineError, RefreshOutcome, TableName, VacuumReport,
};
use serde::{Deserialize, Serialize};

pub(crate) mod refresher;

/// Abandoned `tmp/` files older than this are reclaimed at startup and vacuum.
const TMP_MAX_AGE: Duration = Duration::from_secs(3600);

/// One cache entry serialized to disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub source: String,
    pub table: String,
    pub written_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub row_count: u64,
    pub size_bytes: u64,
    pub file_path: PathBuf,
}

/// JSON manifest listing every cache entry.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Manifest {
    version: u32,
    entries: Vec<ManifestEntry>,
}

/// A live (un-wrapped) provider kept so refresh/invalidate can reach the source
/// by table name alone.
struct RegisteredInner {
    provider: Arc<dyn TableProvider>,
    policy: CachePolicy,
}

/// In-memory + on-disk cache manager.
pub struct CacheManager {
    root: PathBuf,
    manifest: Mutex<Manifest>,
    /// Serializes manifest flushes so a stale snapshot can't clobber a newer one.
    flush_lock: Mutex<()>,
    /// Live providers, keyed by table, for imperative refresh + invalidate.
    inner: Mutex<HashMap<TableName, RegisteredInner>>,
}

impl std::fmt::Debug for CacheManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CacheManager")
            .field("root", &self.root)
            .field("entries", &self.manifest.lock().entries.len())
            .finish()
    }
}

impl CacheManager {
    pub fn new(root: PathBuf) -> std::io::Result<Self> {
        std::fs::create_dir_all(&root)?;
        let manifest = Self::read_manifest_disk(&root);
        // Reclaim any in-progress writes abandoned by a dead process.
        clean_old_tmp(&root, TMP_MAX_AGE);
        Ok(Self {
            root,
            manifest: Mutex::new(manifest),
            flush_lock: Mutex::new(()),
            inner: Mutex::new(HashMap::new()),
        })
    }

    /// Record a live provider so it can be refreshed/invalidated by name.
    pub fn register_inner(
        &self,
        key: TableName,
        provider: Arc<dyn TableProvider>,
        policy: CachePolicy,
    ) {
        self.inner
            .lock()
            .insert(key, RegisteredInner { provider, policy });
    }

    /// Look up a fresh entry by table name, if any.
    fn fresh(&self, name: &TableName) -> Option<ManifestEntry> {
        let now = Utc::now();
        self.manifest
            .lock()
            .entries
            .iter()
            .find(|e| {
                e.source == name.schema
                    && e.table == name.table
                    && e.expires_at.is_none_or(|exp| now < exp)
            })
            .cloned()
    }

    /// Fetch the inner provider fully, materialize it, and write it through to
    /// the cache, bypassing freshness. Used by `EngineService::refresh_table`
    /// and the background refreshers.
    pub async fn refresh(
        &self,
        key: &TableName,
        ctx: &SessionContext,
    ) -> Result<RefreshOutcome, EngineError> {
        let started = Instant::now();
        let (provider, policy) = {
            let inner = self.inner.lock();
            let reg = inner
                .get(key)
                .ok_or_else(|| EngineError::UnknownTable(key.to_string()))?;
            (reg.provider.clone(), reg.policy.clone())
        };

        let state = ctx.state();
        let plan = provider
            .scan(&state, None, &[], None)
            .await
            .map_err(|e| EngineError::Internal(format!("cache refresh scan: {e}")))?;
        let batches = collect(plan, ctx.task_ctx())
            .await
            .map_err(|e| EngineError::Internal(format!("cache refresh collect: {e}")))?;

        let entry = self
            .write_through(key, provider.schema(), &batches, &policy)
            .map_err(|e| EngineError::Internal(format!("cache refresh write: {e}")))?;

        Ok(RefreshOutcome {
            table: key.clone(),
            rows_written: entry.row_count,
            size_bytes: entry.size_bytes,
            elapsed: started.elapsed(),
            expires_at: entry.expires_at,
        })
    }

    /// Drop a cache entry and delete its files. Returns `false` if no entry
    /// existed for the name. Mutates the authoritative on-disk manifest so a
    /// concurrent writer's entries aren't clobbered.
    pub fn invalidate(&self, key: &TableName) -> Result<bool, EngineError> {
        let removed = self
            .with_locked_manifest(|m| {
                m.entries
                    .iter()
                    .position(|e| e.source == key.schema && e.table == key.table)
                    .map(|i| m.entries.remove(i))
            })
            .map_err(|e| EngineError::Internal(format!("cache manifest flush: {e}")))?;
        let Some(entry) = removed else {
            return Ok(false);
        };
        let _ = std::fs::remove_file(&entry.file_path);
        if let Some(parent) = entry.file_path.parent() {
            let _ = std::fs::remove_dir(parent); // only succeeds if now empty
        }
        Ok(true)
    }

    /// Move a corrupt cached file aside to `corrupt/` and drop its manifest
    /// entry, so the next read treats the table as a miss and re-fetches.
    /// Best-effort: if the move fails the file is deleted instead. Called from
    /// the read path when a cached Parquet file fails to open.
    pub fn quarantine(&self, key: &TableName, entry: &ManifestEntry) {
        let _ = self.with_locked_manifest(|m| {
            m.entries.retain(|e| {
                !(e.source == key.schema
                    && e.table == key.table
                    && e.file_path == entry.file_path)
            });
        });
        if !entry.file_path.exists() {
            return;
        }
        let dest_dir = self.root.join("corrupt").join(&key.schema).join(&key.table);
        let moved = std::fs::create_dir_all(&dest_dir).is_ok()
            && entry
                .file_path
                .file_name()
                .map(|fname| {
                    let dest = dest_dir.join(format!(
                        "{}-{}",
                        uuid::Uuid::new_v4(),
                        fname.to_string_lossy()
                    ));
                    std::fs::rename(&entry.file_path, &dest).is_ok()
                })
                .unwrap_or(false);
        if !moved {
            let _ = std::fs::remove_file(&entry.file_path);
        }
    }

    /// Reclaim space: drop expired TTL entries, delete orphaned data files, and
    /// remove stale `tmp/` files.
    pub fn vacuum(&self) -> Result<VacuumReport, EngineError> {
        let mut report = VacuumReport::default();
        let now = Utc::now();

        // 1. Drop expired TTL entries (against the on-disk manifest) and delete
        //    their files.
        let expired: Vec<ManifestEntry> = self
            .with_locked_manifest(|m| {
                let (keep, drop): (Vec<_>, Vec<_>) = std::mem::take(&mut m.entries)
                    .into_iter()
                    .partition(|e| e.expires_at.is_none_or(|exp| now < exp));
                m.entries = keep;
                drop
            })
            .map_err(|e| EngineError::Internal(format!("cache manifest flush: {e}")))?;
        for e in &expired {
            if let Ok(meta) = std::fs::metadata(&e.file_path) {
                report.bytes_reclaimed += meta.len();
            }
            if std::fs::remove_file(&e.file_path).is_ok() {
                report.files_removed += 1;
            }
            if let Some(parent) = e.file_path.parent() {
                let _ = std::fs::remove_dir(parent);
            }
            report.entries_removed += 1;
        }

        // 2. Remove data files not referenced by any surviving entry.
        let referenced: HashSet<PathBuf> = self
            .manifest
            .lock()
            .entries
            .iter()
            .map(|e| e.file_path.clone())
            .collect();
        let mut files = Vec::new();
        collect_files(&self.root.join("data"), &mut files);
        for f in files {
            if referenced.contains(&f) {
                continue;
            }
            if let Ok(meta) = std::fs::metadata(&f) {
                report.bytes_reclaimed += meta.len();
            }
            if std::fs::remove_file(&f).is_ok() {
                report.files_removed += 1;
            }
        }

        // 3. Remove abandoned tmp writes.
        let (tmp_files, tmp_bytes) = clean_old_tmp(&self.root, TMP_MAX_AGE);
        report.files_removed += tmp_files;
        report.bytes_reclaimed += tmp_bytes;

        Ok(report)
    }

    /// Materialize `batches` to Parquet atomically and upsert the manifest.
    fn write_through(
        &self,
        key: &TableName,
        schema: SchemaRef,
        batches: &[RecordBatch],
        policy: &CachePolicy,
    ) -> std::io::Result<ManifestEntry> {
        let final_path = self
            .root
            .join("data")
            .join(&key.schema)
            .join(&key.table)
            .join("part-000000.parquet");
        let size_bytes = atomic_write_parquet(&self.root, &final_path, schema, batches)?;
        let now = Utc::now();
        let entry = ManifestEntry {
            source: key.schema.clone(),
            table: key.table.clone(),
            written_at: now,
            expires_at: ttl_expiry(policy, now),
            row_count: batches.iter().map(|b| b.num_rows() as u64).sum(),
            size_bytes,
            file_path: final_path,
        };
        self.upsert(entry.clone())?;
        Ok(entry)
    }

    /// Replace any existing entry for this name with a new one and persist.
    /// Mutates the authoritative on-disk manifest, so a concurrent writer's
    /// entries survive (no last-writer-wins clobber).
    fn upsert(&self, entry: ManifestEntry) -> std::io::Result<()> {
        self.with_locked_manifest(move |m| {
            m.entries
                .retain(|e| !(e.source == entry.source && e.table == entry.table));
            m.entries.push(entry);
        })
    }

    /// Read-modify-write the on-disk manifest under the cross-process advisory
    /// lock: read the current state from disk (**not** our possibly-stale
    /// in-memory copy), apply `f`, persist atomically, and refresh the in-memory
    /// copy. This is what makes concurrent CLI + daemon writers *merge* rather
    /// than clobber each other — each writer applies its delta on top of
    /// whatever the other already committed.
    fn with_locked_manifest<T>(&self, f: impl FnOnce(&mut Manifest) -> T) -> std::io::Result<T> {
        let _serialize = self.flush_lock.lock(); // serialize in-process writers
        let _file_lock = self.lock_manifest_file()?; // serialize cross-process writers
        let mut manifest = Self::read_manifest_disk(&self.root);
        let out = f(&mut manifest);
        Self::write_manifest_disk(&self.root, &manifest)?;
        *self.manifest.lock() = manifest;
        Ok(out)
    }

    /// Open and exclusively lock `manifest.lock`. Releases when the handle drops.
    fn lock_manifest_file(&self) -> std::io::Result<std::fs::File> {
        let lock_file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.root.join("manifest.lock"))?;
        lock_file.lock_exclusive()?;
        Ok(lock_file)
    }

    /// Read the manifest from disk, or a fresh empty one when absent/unreadable.
    fn read_manifest_disk(root: &Path) -> Manifest {
        let path = root.join("manifest.json");
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or(Manifest {
                version: 1,
                entries: Vec::new(),
            })
    }

    /// Atomically persist a manifest: write to a tmp file, fsync, rename. The
    /// caller must already hold the `manifest.lock` file lock.
    fn write_manifest_disk(root: &Path, manifest: &Manifest) -> std::io::Result<()> {
        let body = serde_json::to_string_pretty(manifest)?;
        let tmp_path = root.join(format!("manifest.json.tmp.{}", uuid::Uuid::new_v4()));
        {
            let mut f = std::fs::File::create(&tmp_path)?;
            f.write_all(body.as_bytes())?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp_path, root.join("manifest.json"))?;
        if let Ok(dir) = std::fs::File::open(root) {
            let _ = dir.sync_all();
        }
        Ok(())
    }

    /// List every entry, for `EngineService::cache_entries`.
    pub fn list(&self) -> Vec<CacheEntryInfo> {
        let entries = self.manifest.lock().entries.clone();
        let inner = self.inner.lock();
        entries
            .iter()
            .map(|e| {
                let name = TableName::new(e.source.clone(), e.table.clone());
                let mode = inner
                    .get(&name)
                    .map(|r| CacheMode::from(&r.policy))
                    .unwrap_or(CacheMode::Ttl);
                CacheEntryInfo {
                    name,
                    mode,
                    written_at: e.written_at,
                    expires_at: e.expires_at,
                    row_count: e.row_count,
                    size_bytes: e.size_bytes,
                    file_count: 1,
                }
            })
            .collect()
    }
}

/// Decorator wrapping any `TableProvider` with cache-aware reads.
#[derive(Debug)]
pub struct CachedTableProvider {
    inner: Arc<dyn TableProvider>,
    name: TableName,
    policy: CachePolicy,
    manager: Arc<CacheManager>,
}

impl CachedTableProvider {
    pub fn wrap(
        inner: Arc<dyn TableProvider>,
        name: TableName,
        policy: CachePolicy,
        manager: Arc<CacheManager>,
    ) -> Arc<dyn TableProvider> {
        if matches!(policy, CachePolicy::None) {
            return inner;
        }
        manager.register_inner(name.clone(), inner.clone(), policy.clone());
        Arc::new(Self {
            inner,
            name,
            policy,
            manager,
        })
    }
}

#[async_trait]
impl TableProvider for CachedTableProvider {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn schema(&self) -> SchemaRef {
        self.inner.schema()
    }

    fn table_type(&self) -> TableType {
        self.inner.table_type()
    }

    fn supports_filters_pushdown(
        &self,
        filters: &[&Expr],
    ) -> datafusion::common::Result<Vec<TableProviderFilterPushDown>> {
        self.inner.supports_filters_pushdown(filters)
    }

    async fn scan(
        &self,
        state: &dyn Session,
        projection: Option<&Vec<usize>>,
        filters: &[Expr],
        limit: Option<usize>,
    ) -> datafusion::common::Result<Arc<dyn ExecutionPlan>> {
        // Cache hit? A corrupt file is quarantined and treated as a miss so the
        // query self-heals by re-fetching rather than failing.
        if let Some(entry) = self.manager.fresh(&self.name) {
            match read_parquet_as_exec(&entry.file_path, projection) {
                Ok(exec) => return Ok(exec),
                Err(e) => {
                    tracing::warn!(
                        error = %e, table = %self.name, path = ?entry.file_path,
                        "cache: corrupt parquet; quarantining and re-fetching"
                    );
                    self.manager.quarantine(&self.name, &entry);
                }
            }
        }

        // Cache miss: fetch live, materialise, write through, then return.
        let inner_plan = self.inner.scan(state, None, filters, limit).await?;
        let runtime = state.task_ctx();
        let batches = collect(inner_plan, runtime).await?;

        if let Err(e) =
            self.manager
                .write_through(&self.name, self.schema(), &batches, &self.policy)
        {
            tracing::warn!(error = %e, "cache: write-through failed; serving live result");
        }

        let projected_batches = project_batches(self.schema(), &batches, projection)?;
        let projected_schema = projected_batches
            .first()
            .map(|b| b.schema())
            .unwrap_or_else(|| self.schema());
        let exec = MemorySourceConfig::try_new_exec(&[projected_batches], projected_schema, None)
            .map_err(|e| DataFusionError::Plan(e.to_string()))?;
        Ok(exec as Arc<dyn ExecutionPlan>)
    }
}

/// `Some(now + ttl)` for TTL mode; `None` otherwise (refresh/cron never expire
/// on read — the background loop keeps them current).
fn ttl_expiry(policy: &CachePolicy, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    match policy {
        CachePolicy::Ttl { ttl } => ChronoDuration::from_std(*ttl).ok().map(|d| now + d),
        _ => None,
    }
}

/// Apply a projection on a list of batches, handling the zero-column case
/// (which `RecordBatch::try_new` rejects without a row-count hint).
fn project_batches(
    schema: SchemaRef,
    batches: &[RecordBatch],
    projection: Option<&Vec<usize>>,
) -> datafusion::common::Result<Vec<RecordBatch>> {
    use arrow_array::RecordBatchOptions;

    let p = match projection {
        Some(p) => p,
        None => return Ok(batches.to_vec()),
    };

    let projected_schema = Arc::new(arrow_schema::Schema::new(
        p.iter()
            .map(|i| schema.field(*i).clone())
            .collect::<Vec<_>>(),
    ));
    let mut out = Vec::with_capacity(batches.len());
    for b in batches {
        if p.is_empty() {
            let opts = RecordBatchOptions::new().with_row_count(Some(b.num_rows()));
            let empty = RecordBatch::try_new_with_options(projected_schema.clone(), vec![], &opts)
                .map_err(|e| DataFusionError::ArrowError(Box::new(e), None))?;
            out.push(empty);
        } else {
            let cols: Vec<arrow_array::ArrayRef> = p.iter().map(|i| b.column(*i).clone()).collect();
            out.push(
                RecordBatch::try_new(projected_schema.clone(), cols)
                    .map_err(|e| DataFusionError::ArrowError(Box::new(e), None))?,
            );
        }
    }
    Ok(out)
}

/// Write Parquet to `tmp/`, fsync, then atomically rename into `final_path`.
/// Returns the size of the finished file.
fn atomic_write_parquet(
    root: &Path,
    final_path: &Path,
    schema: SchemaRef,
    batches: &[RecordBatch],
) -> std::io::Result<u64> {
    let tmp_dir = root.join("tmp");
    std::fs::create_dir_all(&tmp_dir)?;
    let tmp_path = tmp_dir.join(format!("{}.parquet", uuid::Uuid::new_v4()));

    {
        let file = std::fs::File::create(&tmp_path)?;
        let mut writer = ArrowWriter::try_new(file, schema, None).map_err(std::io::Error::other)?;
        for b in batches {
            writer.write(b).map_err(std::io::Error::other)?;
        }
        let file = writer.into_inner().map_err(std::io::Error::other)?;
        file.sync_all()?;
    }

    if let Some(parent) = final_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::rename(&tmp_path, final_path)?;
    if let Some(parent) = final_path.parent() {
        if let Ok(dir) = std::fs::File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    Ok(std::fs::metadata(final_path).map(|m| m.len()).unwrap_or(0))
}

fn read_parquet_as_exec(
    path: &std::path::Path,
    projection: Option<&Vec<usize>>,
) -> std::io::Result<Arc<dyn ExecutionPlan>> {
    let file = std::fs::File::open(path)?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file).map_err(std::io::Error::other)?;
    let schema = builder.schema().clone();
    let reader = builder.build().map_err(std::io::Error::other)?;
    let mut batches = Vec::new();
    for b in reader {
        batches.push(b.map_err(std::io::Error::other)?);
    }

    let projected: Vec<RecordBatch> = project_batches(schema.clone(), &batches, projection)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let projected_schema = projected
        .first()
        .map(|b| b.schema())
        .unwrap_or_else(|| schema.clone());
    let exec = MemorySourceConfig::try_new_exec(&[projected], projected_schema, None)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    Ok(exec as Arc<dyn ExecutionPlan>)
}

/// Recursively collect every file under `dir`.
fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(ft) if ft.is_dir() => collect_files(&path, out),
            Ok(ft) if ft.is_file() => out.push(path),
            _ => {}
        }
    }
}

/// Remove `tmp/` files older than `max_age`. Returns `(files_removed, bytes)`.
fn clean_old_tmp(root: &Path, max_age: Duration) -> (u64, u64) {
    let tmp_dir = root.join("tmp");
    let mut files = 0;
    let mut bytes = 0;
    let Ok(rd) = std::fs::read_dir(&tmp_dir) else {
        return (0, 0);
    };
    for entry in rd.flatten() {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let too_old = meta
            .modified()
            .ok()
            .and_then(|t| t.elapsed().ok())
            .map(|age| age >= max_age)
            .unwrap_or(true);
        if too_old {
            let len = meta.len();
            if std::fs::remove_file(entry.path()).is_ok() {
                files += 1;
                bytes += len;
            }
        }
    }
    (files, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(root: &Path, source: &str, table: &str) -> ManifestEntry {
        ManifestEntry {
            source: source.into(),
            table: table.into(),
            written_at: Utc::now(),
            expires_at: None,
            row_count: 0,
            size_bytes: 0,
            file_path: root
                .join("data")
                .join(source)
                .join(table)
                .join("part-000000.parquet"),
        }
    }

    #[test]
    fn concurrent_writers_merge_instead_of_clobber() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        // Two managers over the same root, each created before the other writes
        // — they hold independent (initially empty) in-memory manifests.
        let a = CacheManager::new(root.clone()).unwrap();
        let b = CacheManager::new(root.clone()).unwrap();

        a.upsert(entry(&root, "s", "alpha")).unwrap();
        // `b` never saw alpha in its in-memory copy; the old wholesale-flush
        // would clobber it. The merge path reads disk first and keeps both.
        b.upsert(entry(&root, "s", "beta")).unwrap();

        let fresh = CacheManager::new(root).unwrap();
        let tables: Vec<String> = fresh.list().into_iter().map(|e| e.name.table).collect();
        assert!(tables.contains(&"alpha".to_string()), "{tables:?}");
        assert!(tables.contains(&"beta".to_string()), "{tables:?}");
    }

    #[test]
    fn invalidate_does_not_resurrect_other_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let mgr = CacheManager::new(root.clone()).unwrap();
        mgr.upsert(entry(&root, "s", "alpha")).unwrap();
        mgr.upsert(entry(&root, "s", "beta")).unwrap();

        assert!(mgr.invalidate(&TableName::new("s", "alpha")).unwrap());

        let fresh = CacheManager::new(root).unwrap();
        let tables: Vec<String> = fresh.list().into_iter().map(|e| e.name.table).collect();
        assert_eq!(tables, vec!["beta".to_string()], "alpha should stay gone");
    }

    #[test]
    fn quarantine_moves_corrupt_file_and_drops_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let mgr = CacheManager::new(root.clone()).unwrap();

        let e = entry(&root, "s", "t");
        std::fs::create_dir_all(e.file_path.parent().unwrap()).unwrap();
        std::fs::write(&e.file_path, b"this is not parquet").unwrap();
        mgr.upsert(e.clone()).unwrap();
        assert!(mgr.fresh(&TableName::new("s", "t")).is_some());

        mgr.quarantine(&TableName::new("s", "t"), &e);

        // Entry dropped, original file gone, one file now under corrupt/.
        assert!(mgr.fresh(&TableName::new("s", "t")).is_none());
        assert!(!e.file_path.exists());
        let corrupt_dir = root.join("corrupt").join("s").join("t");
        let moved = std::fs::read_dir(&corrupt_dir).unwrap().count();
        assert_eq!(moved, 1);
    }
}
