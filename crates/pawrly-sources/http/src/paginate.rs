//! Pagination helpers: parsing `Link` headers, reading cursors out of a JSON
//! body, and computing the next page request per [`PaginationConfig`].
//!
//! These are deliberately pure-ish: they take the just-fetched response pieces
//! (body + headers + rows) and the current request state (params/url), and
//! return the next request to issue — or `None` to stop.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::source::PaginationConfig;

/// The next request to issue after a page, as decided by the pagination logic.
pub enum NextPage {
    /// Re-issue against the table endpoint with these (updated) query params.
    Params(BTreeMap<String, String>),
    /// Follow an absolute URL verbatim (used by `Link` header pagination).
    Url(String),
}

/// Walk a `$.a.b` path into a JSON body and return the leaf value, if present.
///
/// Mirrors the simple walker used by `extract_rows` in `typed.rs`: no filters,
/// slices, or wildcards. `$` returns the body itself.
pub fn json_at_path<'a>(body: &'a Value, path: &str) -> Option<&'a Value> {
    if path == "$" {
        return Some(body);
    }
    let trimmed = path.trim_start_matches("$.");
    let mut current = body;
    for part in trimmed.split('.') {
        if part.is_empty() {
            continue;
        }
        current = current.get(part)?;
    }
    Some(current)
}

/// Read a cursor string out of a JSON body at `path`. Numbers are stringified.
/// Returns `None` when the value is absent, null, or an empty string.
pub fn cursor_at_path(body: &Value, path: &str) -> Option<String> {
    json_at_path(body, path).and_then(value_to_cursor)
}

/// A cursor value from a single JSON value: a non-empty string or a number.
fn value_to_cursor(value: &Value) -> Option<String> {
    match value {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Parse the `Link` response header (RFC 5988) and return the URL whose params
/// include `rel="next"`.
///
/// Honors Sentry's convention of always emitting a `rel="next"` entry and
/// signalling exhaustion with `results="false"` — such an entry is treated as
/// "no next page". GitHub (which simply omits the `next` rel on the last page)
/// carries no `results` attribute, so it is unaffected.
pub fn parse_link_next(headers: &reqwest::header::HeaderMap) -> Option<String> {
    let header = headers.get(reqwest::header::LINK)?.to_str().ok()?;
    // A `Link` header is a comma-separated list of `<url>; rel="next"` entries.
    for part in header.split(',') {
        let mut segments = part.split(';');
        let Some(url_seg) = segments.next() else {
            continue;
        };
        let url = url_seg.trim().trim_start_matches('<').trim_end_matches('>');
        if url.is_empty() {
            continue;
        }
        let mut is_next = false;
        let mut results_false = false;
        for attr in segments {
            let attr = attr.trim();
            // Tolerate `rel=next`, `rel="next"`, `rel='next'` (and `results=`).
            if let Some(v) = attr.strip_prefix("rel=") {
                if v.trim_matches(['"', '\'']) == "next" {
                    is_next = true;
                }
            } else if let Some(v) = attr.strip_prefix("results=") {
                if v.trim_matches(['"', '\'']) == "false" {
                    results_false = true;
                }
            }
        }
        if is_next && !results_false {
            return Some(url.to_string());
        }
    }
    None
}

/// Seed the query params for the *first* page so page/offset schemes send their
/// starting page number / offset (and page size) on the initial request. Cursor
/// and link-header schemes need no seeding (no cursor/link is known yet).
pub fn seed_initial(config: &PaginationConfig, params: &mut BTreeMap<String, String>) {
    match config {
        PaginationConfig::Page {
            param,
            start,
            size_param,
            size,
        } => {
            params
                .entry(param.clone())
                .or_insert_with(|| start.to_string());
            if let (Some(sp), Some(sz)) = (size_param, size) {
                params.entry(sp.clone()).or_insert_with(|| sz.to_string());
            }
        }
        PaginationConfig::Offset {
            param,
            size_param,
            size,
        } => {
            params
                .entry(param.clone())
                .or_insert_with(|| "0".to_string());
            params
                .entry(size_param.clone())
                .or_insert_with(|| size.to_string());
        }
        PaginationConfig::Cursor { .. }
        | PaginationConfig::RowCursor { .. }
        | PaginationConfig::LinkHeader => {}
    }
}

/// Compute the request for the next page given the response we just fetched.
///
/// `params` are the query params used for the *current* request (already
/// includes any cursor/page/offset from the previous step). `page_index` is the
/// zero-based index of the page we just fetched. Returns `None` to stop.
pub fn next_page(
    config: &PaginationConfig,
    params: &BTreeMap<String, String>,
    body: &Value,
    headers: &reqwest::header::HeaderMap,
    rows: &[Value],
    page_index: usize,
) -> Option<NextPage> {
    let row_count = rows.len();
    match config {
        PaginationConfig::LinkHeader => parse_link_next(headers).map(NextPage::Url),
        PaginationConfig::Cursor { next_path, param } => {
            let cursor = cursor_at_path(body, next_path)?;
            let mut next = params.clone();
            next.insert(param.clone(), cursor);
            Some(NextPage::Params(next))
        }
        PaginationConfig::RowCursor {
            param,
            field,
            more_path,
        } => {
            match more_path {
                Some(path) if json_at_path(body, path).and_then(Value::as_bool) != Some(true) => {
                    return None;
                }
                None if rows.is_empty() => return None,
                _ => {}
            }
            let cursor = value_to_cursor(rows.last()?.get(field)?)?;
            let mut next = params.clone();
            next.insert(param.clone(), cursor);
            Some(NextPage::Params(next))
        }
        PaginationConfig::Page {
            param,
            start,
            size_param,
            size,
        } => {
            // Stop when the just-fetched page came back empty.
            if row_count == 0 {
                return None;
            }
            // The next page number = start + (pages fetched so far).
            let next_num = start.saturating_add((page_index as u32).saturating_add(1));
            let mut next = params.clone();
            next.insert(param.clone(), next_num.to_string());
            if let (Some(sp), Some(sz)) = (size_param, size) {
                next.insert(sp.clone(), sz.to_string());
            }
            Some(NextPage::Params(next))
        }
        PaginationConfig::Offset {
            param,
            size_param,
            size,
        } => {
            // Stop on a short (or empty) page.
            if row_count < *size as usize {
                return None;
            }
            let next_offset = (*size as u64).saturating_mul((page_index as u64).saturating_add(1));
            let mut next = params.clone();
            next.insert(param.clone(), next_offset.to_string());
            next.insert(size_param.clone(), size.to_string());
            Some(NextPage::Params(next))
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

    use super::*;
    use reqwest::header::{HeaderMap, HeaderValue, LINK};

    fn link(value: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(LINK, HeaderValue::from_str(value).unwrap());
        h
    }

    #[test]
    fn link_next_github_style() {
        // GitHub: prev + next rels, no `results` attribute.
        let h = link(
            "<https://api.github.com/x?page=1>; rel=\"prev\", \
             <https://api.github.com/x?page=3>; rel=\"next\"",
        );
        assert_eq!(
            parse_link_next(&h).as_deref(),
            Some("https://api.github.com/x?page=3")
        );
    }

    #[test]
    fn link_next_absent_on_last_github_page() {
        let h = link("<https://api.github.com/x?page=1>; rel=\"prev\"");
        assert_eq!(parse_link_next(&h), None);
    }

    #[test]
    fn link_next_sentry_results_true_then_false() {
        // Sentry always emits rel="next"; `results="true"` means follow it,
        // `results="false"` means stop.
        let more = link("<https://sentry.io/x?cursor=abc>; rel=\"next\"; results=\"true\"");
        assert_eq!(
            parse_link_next(&more).as_deref(),
            Some("https://sentry.io/x?cursor=abc")
        );
        let done = link("<https://sentry.io/x?cursor=abc>; rel=\"next\"; results=\"false\"");
        assert_eq!(parse_link_next(&done), None);
    }

    #[test]
    fn row_cursor_advances_then_stops_on_more_flag() {
        let cfg = PaginationConfig::RowCursor {
            param: "starting_after".into(),
            field: "id".into(),
            more_path: Some("$.has_more".into()),
        };
        let params = BTreeMap::new();
        let headers = HeaderMap::new();
        let rows = vec![
            serde_json::json!({ "id": "a" }),
            serde_json::json!({ "id": "b" }),
        ];

        match next_page(
            &cfg,
            &params,
            &serde_json::json!({ "has_more": true }),
            &headers,
            &rows,
            0,
        ) {
            Some(NextPage::Params(p)) => {
                assert_eq!(p.get("starting_after").map(String::as_str), Some("b"));
            }
            _ => panic!("expected a next page"),
        }
        assert!(
            next_page(
                &cfg,
                &params,
                &serde_json::json!({ "has_more": false }),
                &headers,
                &rows,
                1
            )
            .is_none()
        );
    }

    #[test]
    fn cursor_stops_on_empty_or_absent() {
        let body = serde_json::json!({ "response_metadata": { "next_cursor": "" } });
        assert_eq!(
            cursor_at_path(&body, "$.response_metadata.next_cursor"),
            None
        );
        let body2 = serde_json::json!({ "response_metadata": { "next_cursor": "dXNlcjpV" } });
        assert_eq!(
            cursor_at_path(&body2, "$.response_metadata.next_cursor").as_deref(),
            Some("dXNlcjpV")
        );
        let body3 = serde_json::json!({ "ok": true });
        assert_eq!(
            cursor_at_path(&body3, "$.response_metadata.next_cursor"),
            None
        );
    }
}
