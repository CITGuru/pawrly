//! The `explain` / `schema snapshot` / `config reload` commands run and honor
//! `--json`, and `sql --json` == `sql --format json`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use std::process::Command;

use assert_cmd::cargo::cargo_bin;
use tempfile::TempDir;

fn pawrly() -> Command {
    Command::new(cargo_bin("pawrly"))
}

fn minimal_workspace(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("pawrly.yaml");
    std::fs::write(&path, "version: 1\n").unwrap();
    path
}

#[test]
fn explain_json_emits_plan() {
    let tmp = TempDir::new().unwrap();
    let cfg = minimal_workspace(tmp.path());
    let out = pawrly()
        .args([
            "--no-remote",
            "--config",
            cfg.to_str().unwrap(),
            "explain",
            "SELECT 1 AS n",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(
        v["plan"].is_string(),
        "stdout={}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn schema_snapshot_json_is_valid() {
    let tmp = TempDir::new().unwrap();
    let cfg = minimal_workspace(tmp.path());
    let out = pawrly()
        .args([
            "--no-remote",
            "--config",
            cfg.to_str().unwrap(),
            "schema",
            "snapshot",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(
        v["schemas"].is_array(),
        "stdout={}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn reload_config_json_reports_counts() {
    let tmp = TempDir::new().unwrap();
    let cfg = minimal_workspace(tmp.path());
    let out = pawrly()
        .args([
            "--no-remote",
            "--config",
            cfg.to_str().unwrap(),
            "config",
            "reload",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(
        v["sources_added"].is_number(),
        "stdout={}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn sql_json_matches_format_json() {
    let tmp = TempDir::new().unwrap();
    let cfg = minimal_workspace(tmp.path());
    let run = |flag: &[&str]| {
        let out = pawrly()
            .args([
                "--no-remote",
                "--config",
                cfg.to_str().unwrap(),
                "sql",
                "SELECT 1 AS n",
            ])
            .args(flag)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "stderr={}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    let via_json = run(&["--json"]);
    let via_format = run(&["--format", "json"]);
    assert_eq!(via_json, via_format);
    assert!(via_json.contains("\"n\":1"), "stdout={via_json}");
}
