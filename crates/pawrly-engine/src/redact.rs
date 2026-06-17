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
//! degrades (literals → tables-only → nothing) and flags `degraded` so the
//! caller can count it — it never falls back to the raw text.

use std::collections::BTreeSet;
use std::ops::ControlFlow;

use sqlparser::ast::{Expr, Ident, Statement, visit_expressions_mut, visit_relations_mut};
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;

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
            // Degrade to tables-only rather than store raw text.
            None => Redacted {
                sql: tables_only(sql),
                degraded: true,
            },
        },
        RedactMode::TablesOnly => match tables_only(sql) {
            Some(text) => Redacted {
                sql: Some(text),
                degraded: false,
            },
            None => Redacted {
                sql: None,
                degraded: true,
            },
        },
    }
}

fn parse(sql: &str) -> Option<Vec<Statement>> {
    Parser::parse_sql(&GenericDialect, sql).ok()
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
}
