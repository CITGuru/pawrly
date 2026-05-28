//! Strict-M3 acceptance for [POWA-118](/POWA/issues/POWA-118):
//! `pawrly source` add/list/remove round-trip in local mode.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use std::path::{Path, PathBuf};
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

fn write_empty_workspace(dir: &Path) -> PathBuf {
    let path = dir.join("pawrly.yaml");
    std::fs::write(&path, "version: 1\nname: default\nsources: []\n").unwrap();
    path
}

fn run_pawrly(cfg: &Path, args: &[&str]) -> std::process::Output {
    let mut cmd = pawrly();
    cmd.arg("--no-remote").arg("--config").arg(cfg);
    cmd.args(args);
    cmd.output().expect("run pawrly")
}

#[test]
fn source_add_appends_to_yaml() {
    let tmp = TempDir::new().unwrap();
    let cfg = write_empty_workspace(tmp.path());

    // Use a real parquet glob the engine can register without erroring.
    let glob = format!("{}/*.parquet", fixtures_dir().display());

    let add = run_pawrly(
        &cfg,
        &["source", "add", "file", "--name", "data", "--path", &glob],
    );
    let stderr = String::from_utf8_lossy(&add.stderr);
    let stdout = String::from_utf8_lossy(&add.stdout);
    assert!(
        add.status.success(),
        "source add failed: stderr={stderr} stdout={stdout}"
    );

    let yaml_after = std::fs::read_to_string(&cfg).unwrap();
    assert!(
        yaml_after.contains("name: data"),
        "yaml missing source name=data:\n{yaml_after}"
    );
    assert!(
        yaml_after.contains("kind: file"),
        "yaml missing kind=file:\n{yaml_after}"
    );

    let list = run_pawrly(&cfg, &["source", "list", "--json"]);
    let list_stderr = String::from_utf8_lossy(&list.stderr);
    let list_stdout = String::from_utf8_lossy(&list.stdout);
    assert!(
        list.status.success(),
        "source list --json failed: stderr={list_stderr}"
    );
    let parsed: serde_json::Value = serde_json::from_str(&list_stdout).unwrap_or_else(|e| {
        panic!("source list --json was not valid JSON: {e}\nbody:\n{list_stdout}")
    });
    let arr = parsed.as_array().expect("expected JSON array");
    let entry = arr
        .iter()
        .find(|s| s["name"] == "data")
        .expect("`data` not present in source list");
    assert_eq!(entry["kind"], "file", "kind should be file: {entry}");
}

#[test]
fn source_remove_drops_from_yaml() {
    let tmp = TempDir::new().unwrap();
    let cfg = write_empty_workspace(tmp.path());
    let glob = format!("{}/*.parquet", fixtures_dir().display());

    let add = run_pawrly(
        &cfg,
        &["source", "add", "file", "--name", "data", "--path", &glob],
    );
    assert!(add.status.success(), "add failed");

    let rm = run_pawrly(&cfg, &["source", "remove", "data"]);
    let rm_stderr = String::from_utf8_lossy(&rm.stderr);
    assert!(rm.status.success(), "remove failed: {rm_stderr}");

    let yaml_after = std::fs::read_to_string(&cfg).unwrap();
    assert!(
        !yaml_after.contains("name: data"),
        "yaml still contains data after remove:\n{yaml_after}"
    );
}

#[test]
fn source_add_rejects_duplicate() {
    let tmp = TempDir::new().unwrap();
    let cfg = write_empty_workspace(tmp.path());
    let glob = format!("{}/*.parquet", fixtures_dir().display());

    let first = run_pawrly(
        &cfg,
        &["source", "add", "file", "--name", "data", "--path", &glob],
    );
    assert!(first.status.success());

    let second = run_pawrly(
        &cfg,
        &["source", "add", "file", "--name", "data", "--path", &glob],
    );
    assert!(
        !second.status.success(),
        "second add should have failed; stdout={}",
        String::from_utf8_lossy(&second.stdout)
    );
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(
        stderr.contains("already exists"),
        "expected duplicate-name error in stderr: {stderr}"
    );
}
