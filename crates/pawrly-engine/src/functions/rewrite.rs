//! AST pre-rewrite for namespaced function calls.
//!
//! DataFusion's relation planner looks up only the *first* identifier of a
//! table-function name, so `github.search_issues(...)` would be (silently)
//! mis-planned as a UDTF named `github`. Before planning we walk the AST with
//! DataFusion's own re-exported sqlparser and rewrite any **parenthesized**
//! 2-part table factor `ns.fn(args)` into the single mangled UDTF name
//! `pawrly_fn__ns__fn(args)`. A table factor *without* call args is never
//! touched, so `FROM github.issues` stays an ordinary table lookup.

use std::ops::ControlFlow;

use datafusion::sql::sqlparser::ast::{
    Ident, ObjectName, ObjectNamePart, TableFactor, VisitMut, VisitorMut,
};
use datafusion::sql::sqlparser::dialect::GenericDialect;
use datafusion::sql::sqlparser::parser::Parser;
use pawrly_core::EngineError;

use super::{FunctionRegistry, mangle};

/// Rewrite namespaced function calls to their mangled UDTF names. Fails open on
/// a parse error (DataFusion surfaces its own diagnostics, and DF-specific
/// statements plain sqlparser rejects pass through untouched). Returns an error
/// only for an unknown or malformed `ns.fn(...)` call, which is broken inside
/// DataFusion anyway.
pub(crate) fn rewrite_function_calls(
    sql: &str,
    registry: &FunctionRegistry,
) -> Result<String, EngineError> {
    // Fast path: nothing to rewrite without a registry or any call parens.
    if registry.is_empty() || !sql.contains('(') {
        return Ok(sql.to_string());
    }
    let statements = match Parser::parse_sql(&GenericDialect {}, sql) {
        Ok(s) => s,
        Err(_) => return Ok(sql.to_string()),
    };

    let mut visitor = RewriteVisitor {
        registry,
        error: None,
        changed: false,
    };
    let mut parts = Vec::with_capacity(statements.len());
    for mut stmt in statements {
        let _ = stmt.visit(&mut visitor);
        if let Some(e) = visitor.error.take() {
            return Err(e);
        }
        parts.push(stmt.to_string());
    }
    // Only return the round-tripped text when a call was actually rewritten;
    // otherwise hand back the original SQL byte-for-byte, so a query with no
    // function call is never perturbed by the sqlparser `Display` round-trip.
    if visitor.changed {
        Ok(parts.join("; "))
    } else {
        Ok(sql.to_string())
    }
}

struct RewriteVisitor<'a> {
    registry: &'a FunctionRegistry,
    error: Option<EngineError>,
    changed: bool,
}

impl VisitorMut for RewriteVisitor<'_> {
    type Break = ();

    fn pre_visit_table_factor(
        &mut self,
        table_factor: &mut TableFactor,
    ) -> ControlFlow<Self::Break> {
        // Only parenthesized table factors are candidates.
        let TableFactor::Table {
            name,
            args: Some(_),
            ..
        } = table_factor
        else {
            return ControlFlow::Continue(());
        };

        // A non-identifier part (a dialect function part) leaves it untouched.
        let mut idents: Vec<String> = Vec::with_capacity(name.0.len());
        for part in &name.0 {
            match part.as_ident() {
                Some(id) => idents.push(normalize_ident(id)),
                None => return ControlFlow::Continue(()),
            }
        }

        match idents.as_slice() {
            // 1-part: a plain UDTF (including an already-mangled name) — untouched.
            [_] => ControlFlow::Continue(()),
            [ns, func] => {
                if self.registry.contains(ns, func) {
                    *name = ObjectName(vec![ObjectNamePart::Identifier(Ident::new(mangle(
                        ns, func,
                    )))]);
                    self.changed = true;
                    ControlFlow::Continue(())
                } else {
                    self.error = Some(EngineError::UnknownFunction(format!(
                        "{ns}.{func}; declared functions: [{}]",
                        self.registry.declared_names().join(", ")
                    )));
                    ControlFlow::Break(())
                }
            }
            _ => {
                self.error = Some(EngineError::InvalidSql(
                    "function calls support `namespace.function(...)`".to_string(),
                ));
                ControlFlow::Break(())
            }
        }
    }
}

/// Unquoted identifiers fold to lowercase (matching the lowercase mangled name
/// we register); quoted identifiers keep their exact value.
fn normalize_ident(id: &Ident) -> String {
    if id.quote_style.is_some() {
        id.value.clone()
    } else {
        id.value.to_ascii_lowercase()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::functions::test_support::registry_with;

    fn rw(sql: &str) -> Result<String, EngineError> {
        let reg = registry_with(&[("github", "search_issues"), ("file", "glob")]);
        rewrite_function_calls(sql, &reg)
    }

    #[test]
    fn simple_call_is_mangled() {
        let out = rw("SELECT * FROM github.search_issues('is:open', 50)").unwrap();
        assert!(
            out.contains("pawrly_fn__github__search_issues('is:open', 50)"),
            "{out}"
        );
    }

    #[test]
    fn alias_is_preserved() {
        let out = rw("SELECT i.title FROM github.search_issues('x') AS i").unwrap();
        assert!(
            out.contains("pawrly_fn__github__search_issues('x') AS i"),
            "{out}"
        );
    }

    #[test]
    fn join_with_two_factors() {
        let out = rw("SELECT * FROM github.search_issues('x') i JOIN file.glob('*.csv') f ON true")
            .unwrap();
        assert!(
            out.contains("pawrly_fn__github__search_issues('x')"),
            "{out}"
        );
        assert!(out.contains("pawrly_fn__file__glob('*.csv')"), "{out}");
    }

    #[test]
    fn call_inside_cte() {
        let out = rw(
            "WITH hot AS (SELECT * FROM github.search_issues('p0', 20)) SELECT count(*) FROM hot",
        )
        .unwrap();
        assert!(
            out.contains("pawrly_fn__github__search_issues('p0', 20)"),
            "{out}"
        );
    }

    #[test]
    fn bare_two_part_name_untouched() {
        // No parens → ordinary schema.table lookup, never rewritten.
        let out = rw("SELECT * FROM github.issues").unwrap();
        assert_eq!(out, "SELECT * FROM github.issues");
    }

    #[test]
    fn one_part_call_untouched() {
        let out = rw("SELECT * FROM generate_series(1, 5)").unwrap();
        assert!(out.contains("generate_series(1, 5)"), "{out}");
    }

    #[test]
    fn unknown_two_part_call_errors_with_list() {
        let err = rw("SELECT * FROM github.nope('x')").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("github.nope"), "{msg}");
        assert!(msg.contains("github.search_issues"), "{msg}");
    }

    #[test]
    fn three_part_call_errors() {
        let err = rw("SELECT * FROM a.b.c('x')").unwrap_err();
        assert!(err.to_string().contains("namespace.function"), "{err}");
    }

    #[test]
    fn quoted_identifier_is_case_sensitive() {
        // A quoted namespace that doesn't match (case-sensitive) is unknown.
        let err = rw("SELECT * FROM \"GitHub\".search_issues('x')").unwrap_err();
        assert!(err.to_string().contains("GitHub.search_issues"), "{err}");
    }

    #[test]
    fn uppercase_unquoted_is_normalized() {
        let out = rw("SELECT * FROM GitHub.Search_Issues('x')").unwrap();
        assert!(out.contains("pawrly_fn__github__search_issues"), "{out}");
    }

    #[test]
    fn parse_failure_passes_through() {
        let reg = registry_with(&[("github", "search_issues")]);
        let sql = "CREATE EXTERNAL TABLE t STORED AS PARQUET LOCATION '/x'";
        assert_eq!(rewrite_function_calls(sql, &reg).unwrap(), sql);
    }

    #[test]
    fn empty_registry_fast_path() {
        let reg = registry_with(&[]);
        let sql = "SELECT * FROM github.search_issues('x')";
        assert_eq!(rewrite_function_calls(sql, &reg).unwrap(), sql);
    }
}
