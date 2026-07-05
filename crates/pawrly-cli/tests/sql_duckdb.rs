//! `pawrly sql --engine duckdb` runs the query directly on an embedded DuckDB,
//! independent of any workspace config or federated sources.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use std::process::Command;

use assert_cmd::cargo::cargo_bin;

fn pawrly() -> Command {
    Command::new(cargo_bin("pawrly"))
}

#[test]
fn sql_engine_duckdb_runs_without_workspace() {
    let out = pawrly()
        .args([
            "sql",
            "SELECT 42 AS answer",
            "--engine",
            "duckdb",
            "--format",
            "csv",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("answer"), "stdout={stdout}");
    assert!(stdout.contains("42"), "stdout={stdout}");
}

#[test]
fn sql_duckdb_alias_matches_engine_flag() {
    let out = pawrly()
        .args(["sql", "SELECT 7 AS n", "--duckdb", "--format", "csv"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains('7'), "stdout={stdout}");
}
