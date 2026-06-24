//! Builtin/declared `file` function executor. Two shapes, both pure Rust over a
//! glob:
//!   - **glob** (default) → file-metadata rows (`path`, `file_name`,
//!     `size_bytes`, `modified`).
//!   - **grep** (when the body sets a `grep` regex) → one row per matching line
//!     across the matched files (`path`, `line_number`, `line`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use arrow_array::{Int64Array, RecordBatch, StringArray, TimestampMicrosecondArray};
use arrow_schema::{DataType, Field, Schema, SchemaRef, TimeUnit};
use datafusion::common::DataFusionError;
use pawrly_core::{EngineError, FunctionDef};

/// The fixed output schema of a glob `file` function.
fn file_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("path", DataType::Utf8, false),
        Field::new("file_name", DataType::Utf8, true),
        Field::new("size_bytes", DataType::Int64, true),
        Field::new(
            "modified",
            DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
            true,
        ),
    ]))
}

/// The fixed output schema of a grep `file` function.
fn grep_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("path", DataType::Utf8, false),
        Field::new("line_number", DataType::Int64, false),
        Field::new("line", DataType::Utf8, false),
    ]))
}

fn render(template: &str, params: &BTreeMap<String, String>) -> String {
    let mut s = template.to_string();
    for (k, v) in params {
        s = s.replace(&format!("{{{k}}}"), v);
    }
    s
}

/// Executes a `file` function: render `{arg}` placeholders into the glob pattern
/// (and, in grep mode, the regex), resolve against the workspace dir, and emit
/// either file-metadata rows (glob) or matching-line rows (grep).
pub(crate) struct FileGlobExecutor {
    pattern_template: String,
    /// `Some` regex template → grep mode; `None` → glob (metadata) mode.
    grep_template: Option<String>,
    workspace_dir: PathBuf,
    schema: SchemaRef,
}

impl FileGlobExecutor {
    pub(crate) fn new(def: &FunctionDef, workspace_dir: PathBuf) -> Result<Self, EngineError> {
        let pattern_template = def
            .body
            .get("path")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| {
                EngineError::Internal(format!(
                    "file function `{}.{}` is missing a `path` glob",
                    def.namespace, def.name
                ))
            })?
            .to_string();
        let grep_template = def
            .body
            .get("grep")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let schema = if grep_template.is_some() {
            grep_schema()
        } else {
            file_schema()
        };
        Ok(Self {
            pattern_template,
            grep_template,
            workspace_dir,
            schema,
        })
    }

    pub(crate) fn schema(&self) -> SchemaRef {
        self.schema.clone()
    }

    pub(crate) async fn invoke(
        &self,
        params: &BTreeMap<String, String>,
        limit: Option<usize>,
    ) -> datafusion::common::Result<RecordBatch> {
        let resolved =
            resolve_pattern(&self.workspace_dir, &render(&self.pattern_template, params));

        match &self.grep_template {
            // Grep mode: search the matched files' contents.
            Some(grep) => {
                let needle = render(grep, params);
                let re = regex::Regex::new(&needle).map_err(|e| {
                    DataFusionError::Plan(format!("file.grep: invalid pattern `{needle}`: {e}"))
                })?;
                let hits = tokio::task::spawn_blocking(move || grep_files(&resolved, &re, limit))
                    .await
                    .map_err(|e| DataFusionError::Execution(format!("file grep task failed: {e}")))?
                    .map_err(DataFusionError::Execution)?;
                self.build_grep_batch(&hits)
            }
            // Glob mode: file metadata. (glob + fs::metadata is blocking I/O.)
            None => {
                let rows = tokio::task::spawn_blocking(move || glob_metadata(&resolved))
                    .await
                    .map_err(|e| DataFusionError::Execution(format!("file glob task failed: {e}")))?
                    .map_err(DataFusionError::Execution)?;
                let rows = match limit {
                    Some(n) if rows.len() > n => &rows[..n],
                    _ => &rows[..],
                };
                self.build_meta_batch(rows)
            }
        }
    }

    fn build_meta_batch(&self, rows: &[FileMeta]) -> datafusion::common::Result<RecordBatch> {
        let paths: Vec<String> = rows.iter().map(|r| r.path.clone()).collect();
        let names: Vec<Option<String>> = rows.iter().map(|r| r.file_name.clone()).collect();
        let sizes: Vec<Option<i64>> = rows.iter().map(|r| r.size_bytes).collect();
        let modified: Vec<Option<i64>> = rows.iter().map(|r| r.modified_micros).collect();

        let modified_arr =
            TimestampMicrosecondArray::from(modified).with_timezone_opt(Some("UTC".to_string()));

        RecordBatch::try_new(
            self.schema.clone(),
            vec![
                Arc::new(StringArray::from(paths)),
                Arc::new(StringArray::from(names)),
                Arc::new(Int64Array::from(sizes)),
                Arc::new(modified_arr),
            ],
        )
        .map_err(|e| DataFusionError::ArrowError(Box::new(e), None))
    }

    fn build_grep_batch(&self, hits: &[GrepHit]) -> datafusion::common::Result<RecordBatch> {
        let paths: Vec<String> = hits.iter().map(|h| h.path.clone()).collect();
        let nums: Vec<i64> = hits.iter().map(|h| h.line_number).collect();
        let lines: Vec<String> = hits.iter().map(|h| h.line.clone()).collect();

        RecordBatch::try_new(
            self.schema.clone(),
            vec![
                Arc::new(StringArray::from(paths)),
                Arc::new(Int64Array::from(nums)),
                Arc::new(StringArray::from(lines)),
            ],
        )
        .map_err(|e| DataFusionError::ArrowError(Box::new(e), None))
    }
}

struct FileMeta {
    path: String,
    file_name: Option<String>,
    size_bytes: Option<i64>,
    modified_micros: Option<i64>,
}

struct GrepHit {
    path: String,
    line_number: i64,
    line: String,
}

/// Search the matched files' contents line-by-line for `re`, in sorted path
/// order, stopping once `limit` hits are collected. Non-UTF-8 (binary) files are
/// skipped, not an error; a bad glob is an error.
fn grep_files(
    glob_pattern: &str,
    re: &regex::Regex,
    limit: Option<usize>,
) -> Result<Vec<GrepHit>, String> {
    let mut files: Vec<PathBuf> = glob::glob(glob_pattern)
        .map_err(|e| format!("bad glob pattern `{glob_pattern}`: {e}"))?
        .filter_map(Result::ok)
        .filter(|p| p.is_file())
        .collect();
    files.sort();

    let mut out = Vec::new();
    for path in files {
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue; // binary / non-UTF-8
        };
        let path_str = path.to_string_lossy().into_owned();
        for (i, line) in content.lines().enumerate() {
            if re.is_match(line) {
                out.push(GrepHit {
                    path: path_str.clone(),
                    line_number: (i + 1) as i64,
                    line: line.to_string(),
                });
                if limit.is_some_and(|n| out.len() >= n) {
                    return Ok(out);
                }
            }
        }
    }
    Ok(out)
}

/// Resolve a glob pattern: `~`-expand, then make relative patterns workspace-
/// relative (mirrors the file *source* path resolution).
fn resolve_pattern(workspace_dir: &Path, pattern: &str) -> String {
    let expanded = if let Some(rest) = pattern.strip_prefix("~/") {
        match pawrly_core::resolve_home(None) {
            Some(home) => home.join(rest).to_string_lossy().into_owned(),
            None => pattern.to_string(),
        }
    } else {
        pattern.to_string()
    };
    if Path::new(&expanded).is_absolute() || expanded.contains("://") {
        expanded
    } else {
        workspace_dir.join(&expanded).to_string_lossy().into_owned()
    }
}

/// Run the glob and collect metadata, sorted by path. Zero matches → empty (a
/// function call is a query, unlike a source's empty-glob config error). A bad
/// pattern is an error.
fn glob_metadata(pattern: &str) -> Result<Vec<FileMeta>, String> {
    let mut out = Vec::new();
    let paths = glob::glob(pattern).map_err(|e| format!("bad glob pattern `{pattern}`: {e}"))?;
    for entry in paths {
        let Ok(path) = entry else { continue };
        if !path.is_file() {
            continue;
        }
        let meta = std::fs::metadata(&path).ok();
        let size_bytes = meta.as_ref().and_then(|m| i64::try_from(m.len()).ok());
        let modified_micros = meta
            .as_ref()
            .and_then(|m| m.modified().ok())
            .and_then(system_time_micros);
        let file_name = path
            .file_name()
            .and_then(|s| s.to_str())
            .map(str::to_string);
        out.push(FileMeta {
            path: path.to_string_lossy().into_owned(),
            file_name,
            size_bytes,
            modified_micros,
        });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

fn system_time_micros(t: SystemTime) -> Option<i64> {
    t.duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_micros()).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(path: &str) -> FunctionDef {
        FunctionDef {
            namespace: "file".into(),
            name: "glob".into(),
            kind: pawrly_core::FunctionKind::File,
            description: None,
            wiki: None,
            examples: vec![],
            args: vec![],
            returns: vec![],
            connection: serde_json::Value::Null,
            body: serde_json::json!({ "path": path }),
            source: None,
            builtin: true,
            cache: Default::default(),
            safety: None,
        }
    }

    #[tokio::test]
    async fn globs_files_with_metadata_sorted() {
        let dir = std::env::temp_dir().join(format!("pawrly_glob_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("b.csv"), b"hello").unwrap();
        std::fs::write(dir.join("a.csv"), b"hi").unwrap();
        std::fs::write(dir.join("c.txt"), b"nope").unwrap();

        let pattern = format!("{}/*.csv", dir.to_string_lossy());
        let exec = FileGlobExecutor::new(&def(&pattern), dir.clone()).unwrap();
        let batch = exec.invoke(&BTreeMap::new(), None).await.unwrap();

        assert_eq!(batch.num_rows(), 2); // only the two .csv files
        let names = batch
            .column_by_name("file_name")
            .unwrap()
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(names.value(0), "a.csv"); // sorted by path
        assert_eq!(names.value(1), "b.csv");
        let sizes = batch
            .column_by_name("size_bytes")
            .unwrap()
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap();
        assert_eq!(sizes.value(0), 2); // a.csv = "hi"

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn zero_matches_is_empty_not_error() {
        let dir = std::env::temp_dir();
        let pattern = format!("{}/pawrly_no_such_glob_*.zzz", dir.to_string_lossy());
        let exec = FileGlobExecutor::new(&def(&pattern), dir).unwrap();
        let batch = exec.invoke(&BTreeMap::new(), None).await.unwrap();
        assert_eq!(batch.num_rows(), 0);
    }

    #[tokio::test]
    async fn limit_truncates() {
        let dir = std::env::temp_dir().join(format!("pawrly_glob_limit_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        for n in 0..5 {
            std::fs::write(dir.join(format!("f{n}.dat")), b"x").unwrap();
        }
        let pattern = format!("{}/*.dat", dir.to_string_lossy());
        let exec = FileGlobExecutor::new(&def(&pattern), dir.clone()).unwrap();
        let batch = exec.invoke(&BTreeMap::new(), Some(2)).await.unwrap();
        assert_eq!(batch.num_rows(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
