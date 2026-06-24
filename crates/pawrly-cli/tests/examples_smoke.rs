//! Smoke tests for the configs under `examples/`.
//!
//! These guard that the shipped example configs work out of the box. If a
//! contributor adds a new file under `examples/`, they should also add a
//! smoke check here.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use std::path::PathBuf;
use std::process::Command;

use assert_cmd::cargo::cargo_bin;

fn pawrly() -> Command {
    Command::new(cargo_bin("pawrly"))
}

fn examples_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("examples")
}

#[test]
fn file_source_example_runs_schema_clean() {
    let cfg = examples_dir().join("file-source.yaml");
    assert!(
        cfg.exists(),
        "examples/file-source.yaml missing at {}",
        cfg.display()
    );

    let out = pawrly()
        .args(["--no-remote", "--config", cfg.to_str().unwrap(), "schema"])
        .output()
        .expect("schema");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        out.status.success(),
        "schema exited non-zero: stderr={stderr}\nstdout={stdout}"
    );
    assert!(
        !stdout.trim().is_empty(),
        "schema produced empty stdout: stderr={stderr}"
    );
    assert!(
        stdout.contains("data.orders"),
        "expected `data.orders` in schema output, got: {stdout}"
    );
}

#[test]
fn file_source_example_lists_and_queries_its_function() {
    let cfg = examples_dir().join("file-source.yaml");
    let cfg = cfg.to_str().unwrap();

    // The declared function and the builtins show up in `function list`.
    let out = pawrly()
        .args(["--no-remote", "--config", cfg, "function", "list"])
        .output()
        .expect("function list");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "function list non-zero: {stderr}\n{stdout}"
    );
    assert!(
        stdout.contains("fixtures.files"),
        "expected the declared function in list, got: {stdout}"
    );
    assert!(
        stdout.contains("file.glob"),
        "expected the file.glob builtin in list, got: {stdout}"
    );

    // The function is callable from SQL alongside a table, through the CLI.
    let out = pawrly()
        .args([
            "--no-remote",
            "--config",
            cfg,
            "sql",
            "SELECT (SELECT COUNT(*) FROM fixtures.files()) AS files, \
             (SELECT COUNT(*) FROM data.orders) AS orders",
        ])
        .output()
        .expect("sql");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "sql non-zero: {stderr}\n{stdout}");
    assert!(
        stdout.contains("files") && stdout.contains("orders"),
        "expected both columns in output, got: {stdout}"
    );
}

#[test]
fn functions_example_lists_describes_and_runs() {
    let cfg = examples_dir().join("functions.yaml");
    let cfg = cfg.to_str().unwrap();
    // `${secret:GITHUB_TOKEN}` must resolve at load; it is never used by these
    // commands (they don't invoke the github function).
    let token = ("GITHUB_TOKEN", "x");

    // Every declared + builtin function appears in `function list`.
    let out = pawrly()
        .args(["--no-remote", "--config", cfg, "function", "list"])
        .env(token.0, token.1)
        .output()
        .expect("function list");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "function list non-zero: {stderr}\n{stdout}"
    );
    for want in [
        "data.parquet_files",
        "fixtures.csvs",
        "geo.geocode",
        "github.search_issues",
        "file.glob",
        "file.grep",
        "http.get",
    ] {
        assert!(stdout.contains(want), "missing `{want}` in list: {stdout}");
    }

    // `function describe` renders the full spec.
    let out = pawrly()
        .args([
            "--no-remote",
            "--config",
            cfg,
            "function",
            "describe",
            "geo.geocode",
        ])
        .env(token.0, token.1)
        .output()
        .expect("function describe");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "describe non-zero: {stdout}");
    assert!(
        stdout.contains("geo.geocode(address varchar)"),
        "expected the signature in describe, got: {stdout}"
    );

    // The offline file functions execute through the CLI.
    let out = pawrly()
        .args([
            "--no-remote",
            "--config",
            cfg,
            "sql",
            "SELECT file_name FROM fixtures.csvs()",
        ])
        .env(token.0, token.1)
        .output()
        .expect("sql");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "sql non-zero: {stderr}\n{stdout}");
    assert!(
        stdout.contains("customers.csv"),
        "expected fixture file in output, got: {stdout}"
    );
}
