//! Integration tests for `pawrly cache`.
//!
//! These pin down the CLI surface against an empty workspace; the engine-level
//! cache behavior is covered in `pawrly-engine/tests/cache_ttl.rs`.

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

fn write_empty_workspace(dir: &std::path::Path) -> PathBuf {
    let yaml = r#"version: 1
sources: []
"#;
    let path = dir.join("pawrly.yaml");
    std::fs::write(&path, yaml).unwrap();
    path
}

#[test]
fn cache_list_empty_for_fresh_workspace() {
    let tmp = TempDir::new().unwrap();
    let cfg = write_empty_workspace(tmp.path());
    let out = pawrly()
        .args([
            "--no-remote",
            "--config",
            cfg.to_str().unwrap(),
            "cache",
            "list",
            "--json",
        ])
        .output()
        .expect("cache list");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "non-zero exit: stderr={stderr}");
    assert_eq!(
        stdout.trim(),
        "[]",
        "expected empty JSON array, got: {stdout}"
    );
}

#[test]
fn cache_show_reports_missing_entry() {
    let tmp = TempDir::new().unwrap();
    let cfg = write_empty_workspace(tmp.path());
    let out = pawrly()
        .args([
            "--no-remote",
            "--config",
            cfg.to_str().unwrap(),
            "cache",
            "show",
            "data.orders",
        ])
        .output()
        .expect("cache show");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "non-zero exit: stderr={stderr}");
    assert!(
        stdout.contains("no cache entry"),
        "expected a missing-entry message, got: {stdout}"
    );
}

#[test]
fn cache_invalidate_reports_missing_entry() {
    let tmp = TempDir::new().unwrap();
    let cfg = write_empty_workspace(tmp.path());
    let out = pawrly()
        .args([
            "--no-remote",
            "--config",
            cfg.to_str().unwrap(),
            "cache",
            "invalidate",
            "data.orders",
        ])
        .output()
        .expect("cache invalidate");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "non-zero exit: stderr={stderr}");
    assert!(
        stdout.contains("no cache entry"),
        "expected a missing-entry message, got: {stdout}"
    );
}

#[test]
fn cache_vacuum_reports_clean_run() {
    let tmp = TempDir::new().unwrap();
    let cfg = write_empty_workspace(tmp.path());
    let out = pawrly()
        .args([
            "--no-remote",
            "--config",
            cfg.to_str().unwrap(),
            "cache",
            "vacuum",
        ])
        .output()
        .expect("cache vacuum");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "non-zero exit: stderr={stderr}");
    assert!(
        stdout.contains("reclaimed 0 bytes"),
        "expected a vacuum report, got: {stdout}"
    );
}
