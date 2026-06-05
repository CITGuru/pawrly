//! Storage scheme classification for `file` source paths.
//!
//! A `file` source path can be a local path or a remote URL. This module
//! classifies a path by its URL scheme — which decides routing (local
//! DataFusion reader vs DuckDB object-store reader) and which DuckDB secret
//! `TYPE` to synthesize — and extracts the `scheme://authority/` origin used as
//! a secret `SCOPE`.

/// The object-store / transport provider implied by a path's URL scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageScheme {
    Local,
    Http,
    S3,
    Gcs,
    Azure,
}

impl StorageScheme {
    /// Classify a path/URL by its scheme prefix (case-insensitive). A path with
    /// no `://`, or with an unrecognized scheme, is treated as `Local`.
    #[must_use]
    pub fn classify(path: &str) -> StorageScheme {
        let Some((scheme, _)) = path.split_once("://") else {
            return StorageScheme::Local;
        };
        match scheme.to_ascii_lowercase().as_str() {
            "http" | "https" => StorageScheme::Http,
            "s3" => StorageScheme::S3,
            "gs" | "gcs" => StorageScheme::Gcs,
            "az" | "azure" | "abfss" => StorageScheme::Azure,
            _ => StorageScheme::Local,
        }
    }

    /// The `storage.type` string this scheme auto-fills to, or `None` for
    /// `Local`.
    #[must_use]
    pub fn default_storage_type(self) -> Option<&'static str> {
        match self {
            StorageScheme::Local => None,
            StorageScheme::Http => Some("http"),
            StorageScheme::S3 => Some("s3"),
            StorageScheme::Gcs => Some("gcs"),
            StorageScheme::Azure => Some("azure"),
        }
    }

    /// True for any non-local scheme (used by routing).
    #[must_use]
    pub fn is_remote(self) -> bool {
        !matches!(self, StorageScheme::Local)
    }
}

/// The literal `scheme://authority/` prefix of a URL, used as a DuckDB secret
/// `SCOPE`. Authority is everything between `://` and the first `/` or `?`.
/// Returns `None` for local/relative paths (no `://`) or an empty authority.
///
/// Deliberately literal — NOT URL-normalized: DuckDB matches `SCOPE` by string
/// prefix against the request path, and Azure's
/// `abfss://container@account…/` carries the container in the userinfo
/// position, which normalization would reorder or drop. The scheme is preserved
/// as written so the scope prefix-matches the path verbatim.
#[must_use]
pub fn origin_prefix(path: &str) -> Option<String> {
    let (scheme, rest) = path.split_once("://")?;
    let end = rest.find(['/', '?']).unwrap_or(rest.len());
    let authority = &rest[..end];
    if authority.is_empty() {
        return None;
    }
    Some(format!("{scheme}://{authority}/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_schemes() {
        assert_eq!(
            StorageScheme::classify("https://h/a.parquet"),
            StorageScheme::Http
        );
        assert_eq!(
            StorageScheme::classify("http://h/a.csv"),
            StorageScheme::Http
        );
        assert_eq!(StorageScheme::classify("s3://b/k"), StorageScheme::S3);
        assert_eq!(StorageScheme::classify("gs://b/k"), StorageScheme::Gcs);
        assert_eq!(StorageScheme::classify("gcs://b/k"), StorageScheme::Gcs);
        assert_eq!(StorageScheme::classify("az://c/k"), StorageScheme::Azure);
        assert_eq!(StorageScheme::classify("azure://c/k"), StorageScheme::Azure);
        assert_eq!(
            StorageScheme::classify("abfss://c@a/k"),
            StorageScheme::Azure
        );
        assert_eq!(
            StorageScheme::classify("./local/x.parquet"),
            StorageScheme::Local
        );
        assert_eq!(
            StorageScheme::classify("/abs/x.parquet"),
            StorageScheme::Local
        );
        // Unknown scheme → Local (not routed remote).
        assert_eq!(StorageScheme::classify("ftp://h/x"), StorageScheme::Local);
    }

    #[test]
    fn classify_is_case_insensitive() {
        assert_eq!(StorageScheme::classify("HTTPS://h/a"), StorageScheme::Http);
        assert_eq!(StorageScheme::classify("S3://b/k"), StorageScheme::S3);
        assert_eq!(
            StorageScheme::classify("ABFSS://c@a/k"),
            StorageScheme::Azure
        );
    }

    #[test]
    fn default_storage_type_mapping() {
        assert_eq!(StorageScheme::Local.default_storage_type(), None);
        assert_eq!(StorageScheme::Http.default_storage_type(), Some("http"));
        assert_eq!(StorageScheme::S3.default_storage_type(), Some("s3"));
        assert_eq!(StorageScheme::Gcs.default_storage_type(), Some("gcs"));
        assert_eq!(StorageScheme::Azure.default_storage_type(), Some("azure"));
    }

    #[test]
    fn is_remote_only_local_false() {
        assert!(!StorageScheme::Local.is_remote());
        for s in [
            StorageScheme::Http,
            StorageScheme::S3,
            StorageScheme::Gcs,
            StorageScheme::Azure,
        ] {
            assert!(s.is_remote());
        }
    }

    #[test]
    fn origin_prefix_examples() {
        assert_eq!(
            origin_prefix("https://h:8443/a/b?sig=x").as_deref(),
            Some("https://h:8443/")
        );
        assert_eq!(
            origin_prefix("https://h?x=1").as_deref(),
            Some("https://h/")
        );
        assert_eq!(
            origin_prefix("s3://bucket/key").as_deref(),
            Some("s3://bucket/")
        );
        assert_eq!(
            origin_prefix("gs://bucket/k").as_deref(),
            Some("gs://bucket/")
        );
        assert_eq!(
            origin_prefix("az://container/k").as_deref(),
            Some("az://container/")
        );
        // Azure abfss keeps the container in the userinfo position.
        assert_eq!(
            origin_prefix("abfss://container@acct.dfs.core.windows.net/k").as_deref(),
            Some("abfss://container@acct.dfs.core.windows.net/"),
        );
        // Scheme preserved verbatim for literal prefix matching.
        assert_eq!(
            origin_prefix("S3://bucket/k").as_deref(),
            Some("S3://bucket/")
        );
        // Local / degenerate → None.
        assert_eq!(origin_prefix("./local/x.parquet"), None);
        assert_eq!(origin_prefix("https:///nohost"), None);
    }
}
