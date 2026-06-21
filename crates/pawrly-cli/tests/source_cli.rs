//! Acceptance: `pawrly source` add/list/remove round-trip in local mode.

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
fn source_add_writes_per_source_file() {
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

    // The source lands in its own file under sources/, not inline in the root.
    let source_file = tmp.path().join("sources").join("data.yaml");
    let source_yaml = std::fs::read_to_string(&source_file)
        .unwrap_or_else(|e| panic!("sources/data.yaml not written: {e}"));
    assert!(
        source_yaml.contains("name: data"),
        "sources/data.yaml missing name=data:\n{source_yaml}"
    );
    assert!(
        source_yaml.contains("kind: file"),
        "sources/data.yaml missing kind=file:\n{source_yaml}"
    );

    // The root manifest wires the per-source files in via an include glob.
    let root_yaml = std::fs::read_to_string(&cfg).unwrap();
    assert!(
        root_yaml.contains("sources/*.yaml"),
        "root missing sources include glob:\n{root_yaml}"
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
fn source_remove_deletes_file_and_drops_glob() {
    let tmp = TempDir::new().unwrap();
    let cfg = write_empty_workspace(tmp.path());
    let glob = format!("{}/*.parquet", fixtures_dir().display());

    let add = run_pawrly(
        &cfg,
        &["source", "add", "file", "--name", "data", "--path", &glob],
    );
    assert!(add.status.success(), "add failed");

    let source_file = tmp.path().join("sources").join("data.yaml");
    assert!(
        source_file.exists(),
        "sources/data.yaml should exist after add"
    );

    let rm = run_pawrly(&cfg, &["source", "remove", "data"]);
    let rm_stderr = String::from_utf8_lossy(&rm.stderr);
    assert!(rm.status.success(), "remove failed: {rm_stderr}");

    assert!(
        !source_file.exists(),
        "sources/data.yaml should be deleted after remove"
    );
    // The now-empty glob is dropped so the next load doesn't fail on it.
    let root_yaml = std::fs::read_to_string(&cfg).unwrap();
    assert!(
        !root_yaml.contains("sources/*.yaml"),
        "root should drop the sources glob once no per-source file remains:\n{root_yaml}"
    );
}

#[test]
fn source_add_from_local_file_imports_spec() {
    let tmp = TempDir::new().unwrap();
    let cfg = write_empty_workspace(tmp.path());
    let glob = format!("{}/*.parquet", fixtures_dir().display());

    // A bare single-source spec on disk (the catalog file shape).
    let spec = tmp.path().join("spec.yaml");
    std::fs::write(
        &spec,
        format!("name: imported\nkind: file\nconfig:\n  path: {glob}\n"),
    )
    .unwrap();

    let add = run_pawrly(&cfg, &["source", "add", spec.to_str().unwrap()]);
    let stderr = String::from_utf8_lossy(&add.stderr);
    assert!(
        add.status.success(),
        "import-from-file add failed: {stderr}"
    );

    // Lands as its own per-source file, named from the spec's own `name:`.
    let imported = tmp.path().join("sources").join("imported.yaml");
    let body = std::fs::read_to_string(&imported)
        .unwrap_or_else(|e| panic!("sources/imported.yaml missing: {e}"));
    assert!(body.contains("name: imported"), "body:\n{body}");
    assert!(body.contains("kind: file"), "body:\n{body}");
}

#[test]
fn source_add_from_file_with_rename() {
    let tmp = TempDir::new().unwrap();
    let cfg = write_empty_workspace(tmp.path());
    let glob = format!("{}/*.parquet", fixtures_dir().display());
    let spec = tmp.path().join("spec.yaml");
    std::fs::write(
        &spec,
        format!("name: imported\nkind: file\nconfig:\n  path: {glob}\n"),
    )
    .unwrap();

    let add = run_pawrly(
        &cfg,
        &["source", "add", spec.to_str().unwrap(), "--name", "renamed"],
    );
    assert!(add.status.success(), "rename import failed");

    assert!(
        tmp.path().join("sources").join("renamed.yaml").exists(),
        "expected the renamed file"
    );
    assert!(
        !tmp.path().join("sources").join("imported.yaml").exists(),
        "should not also write the original name"
    );
}

#[test]
fn source_add_rejects_build_flags_with_import() {
    let tmp = TempDir::new().unwrap();
    let cfg = write_empty_workspace(tmp.path());
    let spec = tmp.path().join("spec.yaml");
    std::fs::write(&spec, "name: x\nkind: file\nconfig:\n  path: ./a.parquet\n").unwrap();

    let out = run_pawrly(
        &cfg,
        &[
            "source",
            "add",
            spec.to_str().unwrap(),
            "--path",
            "./b.parquet",
        ],
    );
    assert!(
        !out.status.success(),
        "build flags + import should be rejected"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("can't be combined"),
        "expected combine error: {stderr}"
    );
}

#[test]
fn source_add_no_verify_skips_validation() {
    let tmp = TempDir::new().unwrap();
    let cfg = write_empty_workspace(tmp.path());

    // An http source with only base_url — no live engine check, so --no-verify
    // writes it without touching the network, and --url lands as base_url.
    let add = run_pawrly(
        &cfg,
        &[
            "source",
            "add",
            "http",
            "--name",
            "gh",
            "--url",
            "https://api.github.com",
            "--no-verify",
        ],
    );
    let stderr = String::from_utf8_lossy(&add.stderr);
    assert!(add.status.success(), "--no-verify add failed: {stderr}");

    let body = std::fs::read_to_string(tmp.path().join("sources").join("gh.yaml")).unwrap();
    assert!(
        body.contains("base_url: https://api.github.com"),
        "http --url should map to base_url:\n{body}"
    );
}

#[test]
fn source_add_bootstraps_home_workspace() {
    // No --config, no ./pawrly.yaml in cwd: the writer must fall back to
    // $PAWRLY_HOME and bootstrap <home>/pawrly.yaml + <home>/sources/<name>.yaml.
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let work = tmp.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let glob = format!("{}/*.parquet", fixtures_dir().display());

    let out = pawrly()
        .current_dir(&work)
        .env("PAWRLY_HOME", &home)
        .env_remove("PAWRLY_CONFIG")
        .args([
            "--no-remote",
            "source",
            "add",
            "file",
            "--name",
            "data",
            "--path",
            &glob,
        ])
        .output()
        .expect("run pawrly");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "home-bootstrap add failed: stderr={stderr} stdout={stdout}"
    );

    assert!(
        home.join("pawrly.yaml").exists(),
        "expected $PAWRLY_HOME/pawrly.yaml to be bootstrapped"
    );
    assert!(
        home.join("sources").join("data.yaml").exists(),
        "expected $PAWRLY_HOME/sources/data.yaml to be written"
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
