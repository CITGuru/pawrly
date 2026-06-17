//! SQL redaction for the activity log.
//!
//! Redaction runs on the SQL the user wrote, before `${param:..}` substitution,
//! so param values can never leak. Three modes:
//!
//! - [`RedactMode::Off`] — store verbatim.
//! - [`RedactMode::Literals`] — parse to an AST, replace every literal with the
//!   non-executable sentinel `$REDACTED`, and re-serialize. Shape is kept,
//!   values dropped.
//! - [`RedactMode::TablesOnly`] — store only the statement kind plus referenced
//!   tables, never the text.
//!
//! Redaction never fails open: if a redacting mode cannot parse the SQL, it
//! degrades (literals → tables-only → the leading statement keyword) and flags
//! `degraded` so the caller can count it — it never falls back to the raw text.
//!
//! Parsing uses DataFusion's own re-exported `sqlparser`, so the redactor
//! accepts exactly the grammar the engine runs.

use std::collections::BTreeSet;
use std::ops::ControlFlow;

use datafusion::sql::sqlparser::ast::{
    Expr, Ident, Statement, visit_expressions_mut, visit_relations_mut,
};
use datafusion::sql::sqlparser::dialect::GenericDialect;
use datafusion::sql::sqlparser::parser::Parser;

/// Non-executable placeholder for redacted literals. Deliberately not valid SQL
/// so the stored text is never mistaken for runnable.
const SENTINEL: &str = "$REDACTED";

/// How much of the submitted SQL to capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RedactMode {
    /// Store SQL verbatim.
    #[default]
    Off,
    /// Replace literal values with `$REDACTED`, keeping query shape.
    Literals,
    /// Store only the statement kind and referenced tables.
    TablesOnly,
}

/// Outcome of redacting one statement string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redacted {
    /// Text to store, or `None` when no text should be stored.
    pub sql: Option<String>,
    /// True when the requested mode could not be fully applied and we degraded.
    pub degraded: bool,
}

/// Redact `sql` according to `mode`.
pub fn redact(sql: &str, mode: RedactMode) -> Redacted {
    match mode {
        RedactMode::Off => Redacted {
            sql: Some(sql.to_string()),
            degraded: false,
        },
        RedactMode::Literals => match parse(sql) {
            Some(stmts) => Redacted {
                sql: Some(redact_literals(stmts)),
                degraded: false,
            },
            // Degrade to tables-only, then to the bare statement keyword,
            // rather than store raw text.
            None => Redacted {
                sql: tables_only(sql).or_else(|| leading_keyword(sql)),
                degraded: true,
            },
        },
        RedactMode::TablesOnly => match tables_only(sql) {
            Some(text) => Redacted {
                sql: Some(text),
                degraded: false,
            },
            None => Redacted {
                sql: leading_keyword(sql),
                degraded: true,
            },
        },
    }
}

fn parse(sql: &str) -> Option<Vec<Statement>> {
    Parser::parse_sql(&GenericDialect, sql).ok()
}

/// Statement-leading keywords safe to record on a parse failure — a keyword
/// can't carry user data, so this leaks nothing.
const STMT_KEYWORDS: &[&str] = &[
    "SELECT", "WITH", "INSERT", "UPDATE", "DELETE", "CREATE", "DROP", "ALTER", "EXPLAIN", "SHOW",
    "COPY", "MERGE", "CALL", "SET", "PRAGMA", "DESCRIBE",
];

/// The leading word of `sql`, but only when it's a recognized statement keyword;
/// otherwise `None`, so a malformed input never echoes a non-keyword token.
fn leading_keyword(sql: &str) -> Option<String> {
    let word: String = sql
        .trim_start()
        .chars()
        .take_while(char::is_ascii_alphabetic)
        .collect::<String>()
        .to_uppercase();
    STMT_KEYWORDS.contains(&word.as_str()).then_some(word)
}

/// Replace every literal expression with the sentinel and re-serialize.
fn redact_literals(mut stmts: Vec<Statement>) -> String {
    for stmt in &mut stmts {
        let _ = visit_expressions_mut(stmt, |expr| {
            if matches!(expr, Expr::Value(_)) {
                *expr = Expr::Identifier(Ident::new(SENTINEL));
            }
            ControlFlow::<()>::Continue(())
        });
    }
    stmts
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

/// Produce `"<KIND> on <tables>"` from a parsed statement, or `None` if the SQL
/// does not parse.
fn tables_only(sql: &str) -> Option<String> {
    let mut stmts = parse(sql)?;
    let mut kinds = Vec::new();
    let mut tables = BTreeSet::new();
    for stmt in &mut stmts {
        kinds.push(stmt_kind(stmt));
        let _ = visit_relations_mut(stmt, |name| {
            tables.insert(name.to_string());
            ControlFlow::<()>::Continue(())
        });
    }
    let kind = kinds.join(",");
    if tables.is_empty() {
        Some(kind)
    } else {
        let names = tables.into_iter().collect::<Vec<_>>().join(", ");
        Some(format!("{kind} on {names}"))
    }
}

fn stmt_kind(stmt: &Statement) -> &'static str {
    match stmt {
        Statement::Query(_) => "SELECT",
        Statement::Explain { .. } => "EXPLAIN",
        Statement::Insert(_) => "INSERT",
        _ => "STATEMENT",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_is_verbatim() {
        let sql = "SELECT * FROM orders WHERE email = 'a@b.com'";
        assert_eq!(redact(sql, RedactMode::Off).sql.as_deref(), Some(sql));
    }

    #[test]
    fn literals_replaces_values_keeps_shape() {
        let r = redact(
            "SELECT * FROM orders WHERE email = 'toby@finic.ai' AND total > 500",
            RedactMode::Literals,
        );
        let out = r.sql.unwrap();
        assert!(!r.degraded);
        assert!(out.contains("$REDACTED"), "expected sentinel in {out}");
        assert!(!out.contains("toby@finic.ai"), "value leaked: {out}");
        assert!(!out.contains("500"), "value leaked: {out}");
        // Shape preserved: columns/tables intact.
        assert!(out.contains("orders"));
        assert!(out.contains("email"));
    }

    #[test]
    fn tables_only_drops_predicate() {
        let r = redact(
            "SELECT a, b FROM orders WHERE email = 'x'",
            RedactMode::TablesOnly,
        );
        let out = r.sql.unwrap();
        assert!(!r.degraded);
        assert_eq!(out, "SELECT on orders");
        assert!(!out.contains("email"));
    }

    #[test]
    fn literals_failsafe_does_not_leak_on_parse_error() {
        // Not valid SQL: must not return the raw text.
        let raw = "this is 'secret' not sql @@@";
        let r = redact(raw, RedactMode::Literals);
        assert!(r.degraded);
        assert_ne!(r.sql.as_deref(), Some(raw));
        if let Some(text) = &r.sql {
            assert!(!text.contains("secret"), "value leaked on failure: {text}");
        }
    }

    #[test]
    fn tables_only_failsafe_returns_none() {
        let r = redact("@@@ not sql", RedactMode::TablesOnly);
        assert!(r.degraded);
        assert_eq!(r.sql, None);
    }

    #[test]
    fn modern_syntax_parses_via_datafusion_dialect() {
        // `FILTER` is valid in DataFusion and must redact, not degrade to NULL.
        let r = redact(
            "SELECT count(*) FILTER (WHERE n > 0) FROM t",
            RedactMode::Literals,
        );
        assert!(!r.degraded, "FILTER should parse with the engine's dialect");
        let out = r.sql.expect("expected redacted SQL, not NULL");
        assert!(out.contains("FILTER"), "shape lost: {out}");
        assert!(out.contains("$REDACTED"), "literal not redacted: {out}");
    }

    #[test]
    fn unparseable_falls_back_to_leading_keyword() {
        let r = redact("SELECT this is not valid ((", RedactMode::Literals);
        assert!(r.degraded);
        // Leak-safe keyword, never the raw text.
        assert_eq!(r.sql.as_deref(), Some("SELECT"));
    }

    #[test]
    fn non_keyword_garbage_yields_no_text() {
        let r = redact("@@@ totally bogus", RedactMode::Literals);
        assert!(r.degraded);
        assert_eq!(r.sql, None);
    }
}
