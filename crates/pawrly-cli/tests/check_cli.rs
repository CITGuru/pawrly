//! Acceptance: `pawrly check` runs each source's `examples:` statements as
//! live probes and fails on a broken one.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use std::path::PathBuf;
use std::process::Command;

use assert_cmd::cargo::cargo_bin;
use tempfile::TempDir;

fn pawrly() -> Command {
    Command::new(cargo_bin("pawrly"))
}

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

fn write_workspace(dir: &std::path::Path, examples_yaml: &str) -> PathBuf {
    let fx = fixtures_dir();
    let yaml = format!(
        r#"version: 1
sources:
  - name: data
    kind: file
{examples_yaml}    config:
      path: "{}"
    tables:
      - name: orders
        path: "{}"
        format: parquet
"#,
        fx.display(),
        fx.join("orders.parquet").display(),
    );
    let path = dir.join("pawrly.yaml");
    std::fs::write(&path, yaml).unwrap();
    path
}

#[test]
fn check_passes_on_good_examples() {
    let tmp = TempDir::new().unwrap();
    let cfg = write_workspace(
        tmp.path(),
        "    examples:\n      - SELECT * FROM data.orders LIMIT 1\n",
    );
    let out = pawrly()
        .args(["--no-remote", "--config", cfg.to_str().unwrap(), "check"])
        .output()
        .expect("check");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stderr={stderr} stdout={stdout}");
    assert!(stdout.contains("1 passed, 0 failed"), "stdout={stdout}");
}

#[test]
fn check_fails_on_broken_example() {
    let tmp = TempDir::new().unwrap();
    let cfg = write_workspace(
        tmp.path(),
        "    examples:\n      - SELECT * FROM data.orders LIMIT 1\n      - SELECT * FROM data.nope LIMIT 1\n",
    );
    let out = pawrly()
        .args(["--no-remote", "--config", cfg.to_str().unwrap(), "check"])
        .output()
        .expect("check");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!out.status.success(), "expected failure: stdout={stdout}");
    assert!(stdout.contains("1 passed, 1 failed"), "stdout={stdout}");
    assert!(
        stdout.contains("FAIL SELECT * FROM data.nope"),
        "stdout={stdout}"
    );
}

#[test]
fn check_reports_nothing_to_do() {
    let tmp = TempDir::new().unwrap();
    let cfg = write_workspace(tmp.path(), "");
    let out = pawrly()
        .args(["--no-remote", "--config", cfg.to_str().unwrap(), "check"])
        .output()
        .expect("check");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "stdout={stdout}");
    assert!(stdout.contains("no examples declared"), "stdout={stdout}");
}
