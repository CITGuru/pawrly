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
    /// Re-issue against the table endpoint, injecting this cursor into the
    /// request body at the configured path (used by body-cursor pagination).
    BodyCursor(String),
}

/// Set the value at a `$.a.b` path inside a JSON object, creating intermediate
/// objects as needed. Returns `false` if a non-object blocks the path (the body
/// is then left unchanged). `$` replaces the whole value.
pub fn set_json_at_path(root: &mut Value, path: &str, leaf: Value) -> bool {
    let trimmed = path.trim_start_matches("$.").trim_start_matches('$');
    let parts: Vec<&str> = trimmed.split('.').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        *root = leaf;
        return true;
    }
    let mut cur = root;
    for part in &parts[..parts.len() - 1] {
        match cur.as_object_mut() {
            Some(obj) => {
                cur = obj
                    .entry((*part).to_string())
                    .or_insert_with(|| Value::Object(serde_json::Map::new()));
            }
            None => return false,
        }
    }
    match (cur.as_object_mut(), parts.last()) {
        (Some(obj), Some(last)) => {
            obj.insert((*last).to_string(), leaf);
            true
        }
        _ => false,
    }
}

/// Walk a `$.a.b` path into a JSON body and return the leaf value, if present.
///
/// Supports object keys and array indices, including bare leading indices, so
/// `$.data`, `$.data[0]`, `$[1]`, and `$.items[2].name` all resolve. No filters,
/// slices, or wildcards. `$` returns the body itself.
pub fn json_at_path<'a>(body: &'a Value, path: &str) -> Option<&'a Value> {
    if path == "$" {
        return Some(body);
    }
    let trimmed = path.trim_start_matches("$.").trim_start_matches('$');
    let mut current = body;
    for part in trimmed.split('.') {
        if part.is_empty() {
            continue;
        }
        current = segment_get(current, part)?;
    }
    Some(current)
}

/// Resolve one dot-delimited path segment that may carry trailing array indices,
/// e.g. `data`, `data[0]`, `[1]`, or `items[2][3]`.
fn segment_get<'a>(value: &'a Value, segment: &str) -> Option<&'a Value> {
    let (key, mut rest) = match segment.find('[') {
        Some(i) => (&segment[..i], &segment[i..]),
        None => (segment, ""),
    };
    let mut current = if key.is_empty() {
        value
    } else {
        value.get(key)?
    };
    while let Some(stripped) = rest.strip_prefix('[') {
        let end = stripped.find(']')?;
        let idx: usize = stripped[..end].trim().parse().ok()?;
        current = current.get(idx)?;
        rest = &stripped[end + 1..];
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
        | PaginationConfig::BodyCursor { .. }
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
            // Stop on an empty page, when no further cursor is offered, or when
            // the cursor is unchanged (a repeating cursor would loop until the
            // page cap).
            if rows.is_empty() {
                return None;
            }
            let cursor = cursor_at_path(body, next_path)?;
            if params.get(param) == Some(&cursor) {
                return None;
            }
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
        PaginationConfig::BodyCursor { next_path, .. } => {
            // Stop on an empty page or when no further cursor is offered.
            if rows.is_empty() {
                return None;
            }
            let cursor = cursor_at_path(body, next_path)?;
            Some(NextPage::BodyCursor(cursor))
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
    fn set_json_at_path_inserts_and_creates() {
        let mut v = serde_json::json!({ "variables": { "first": 100 } });
        assert!(set_json_at_path(
            &mut v,
            "$.variables.after",
            Value::String("CUR".into())
        ));
        assert_eq!(v["variables"]["after"], serde_json::json!("CUR"));
        assert_eq!(v["variables"]["first"], serde_json::json!(100));

        let mut w = serde_json::json!({});
        assert!(set_json_at_path(&mut w, "$.a.b", Value::String("x".into())));
        assert_eq!(w["a"]["b"], serde_json::json!("x"));

        // A non-object blocking the path is rejected, leaving the value intact.
        let mut blocked = serde_json::json!({ "a": 1 });
        assert!(!set_json_at_path(&mut blocked, "$.a.b", Value::Bool(true)));
        assert_eq!(blocked["a"], serde_json::json!(1));
    }

    #[test]
    fn json_at_path_resolves_keys_and_indices() {
        let body = serde_json::json!([
            { "page": 1 },
            [{ "country": { "value": "USA" } }, { "country": { "value": "NGA" } }]
        ]);
        // Whole body.
        assert_eq!(json_at_path(&body, "$"), Some(&body));
        // Bare leading index into the top-level array.
        assert_eq!(
            json_at_path(&body, "$[1]"),
            Some(&serde_json::json!([
                { "country": { "value": "USA" } },
                { "country": { "value": "NGA" } }
            ]))
        );
        // Index then key chain.
        assert_eq!(
            json_at_path(&body, "$[1][0].country.value"),
            Some(&serde_json::json!("USA"))
        );
        // key[idx] form.
        let wrapped = serde_json::json!({ "data": [10, 20, 30] });
        assert_eq!(
            json_at_path(&wrapped, "$.data[2]"),
            Some(&serde_json::json!(30))
        );
        // Out-of-bounds / missing -> None.
        assert!(json_at_path(&body, "$[9]").is_none());
        assert!(json_at_path(&wrapped, "$.missing[0]").is_none());
    }

    #[test]
    fn body_cursor_advances_then_stops() {
        let cfg = PaginationConfig::BodyCursor {
            cursor_path: "$.variables.after".into(),
            next_path: "$.data.teams.pageInfo.endCursor".into(),
        };
        let params = BTreeMap::new();
        let headers = HeaderMap::new();
        let rows = vec![serde_json::json!({ "id": "a" })];
        let body =
            serde_json::json!({ "data": { "teams": { "pageInfo": { "endCursor": "CUR" } } } });

        match next_page(&cfg, &params, &body, &headers, &rows, 0) {
            Some(NextPage::BodyCursor(c)) => assert_eq!(c, "CUR"),
            _ => panic!("expected a body cursor"),
        }
        // Empty cursor -> stop.
        let empty = serde_json::json!({ "data": { "teams": { "pageInfo": { "endCursor": "" } } } });
        assert!(next_page(&cfg, &params, &empty, &headers, &rows, 1).is_none());
        // Empty page -> stop regardless of cursor.
        assert!(next_page(&cfg, &params, &body, &headers, &[], 1).is_none());
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
    fn cursor_advances_then_stops_when_unchanged() {
        let cfg = PaginationConfig::Cursor {
            next_path: "$.meta.next".into(),
            param: "cursor".into(),
        };
        let headers = HeaderMap::new();
        let rows = vec![serde_json::json!({ "id": 1 })];

        // First page (no cursor yet) advances to "c2".
        let params = BTreeMap::new();
        match next_page(
            &cfg,
            &params,
            &serde_json::json!({ "meta": { "next": "c2" } }),
            &headers,
            &rows,
            0,
        ) {
            Some(NextPage::Params(p)) => {
                assert_eq!(p.get("cursor").map(String::as_str), Some("c2"))
            }
            _ => panic!("expected a next page"),
        }

        // A response echoing the cursor we already sent must stop, not loop.
        let mut same = BTreeMap::new();
        same.insert("cursor".to_string(), "c2".to_string());
        assert!(
            next_page(
                &cfg,
                &same,
                &serde_json::json!({ "meta": { "next": "c2" } }),
                &headers,
                &rows,
                1,
            )
            .is_none(),
            "an unchanged cursor should stop pagination"
        );

        // An empty page also stops.
        assert!(
            next_page(
                &cfg,
                &params,
                &serde_json::json!({ "meta": { "next": "c3" } }),
                &headers,
                &[],
                1,
            )
            .is_none(),
            "an empty page should stop cursor pagination"
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
