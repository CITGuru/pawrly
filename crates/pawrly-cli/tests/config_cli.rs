//! Acceptance for `pawrly config show` (--raw / --tree) and the source-origin
//! annotation on `pawrly source list`.

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

fn run(cfg: &Path, args: &[&str]) -> std::process::Output {
    let mut cmd = pawrly();
    cmd.arg("--no-remote").arg("--config").arg(cfg);
    cmd.args(args);
    cmd.output().expect("run pawrly")
}

/// Root config that includes a fragment and has one inline source carrying a
/// secret reference.
fn write_multi_file_workspace(dir: &Path) -> PathBuf {
    let root = dir.join("pawrly.yaml");
    std::fs::write(
        &root,
        "version: 1\nname: smoke\ninclude:\n  - ./team.yaml\nsources:\n  - name: gh_root\n    kind: github\n    config:\n      token: ${secret:GH_TOKEN}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("team.yaml"),
        "sources:\n  - name: gh_team\n    kind: github\n    config:\n      token: literal\n",
    )
    .unwrap();
    root
}

#[test]
fn config_show_assembles_and_keeps_secret_refs_verbatim() {
    let tmp = TempDir::new().unwrap();
    let cfg = write_multi_file_workspace(tmp.path());

    let out = run(&cfg, &["config", "show"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);

    // Both the inline and the included source are present (assembled).
    assert!(stdout.contains("gh_root"), "missing gh_root:\n{stdout}");
    assert!(stdout.contains("gh_team"), "missing gh_team:\n{stdout}");
    // `include:` was consumed by assembly, not echoed.
    assert!(
        !stdout.contains("include:"),
        "include: should be gone:\n{stdout}"
    );
    // Secret reference is shown verbatim — never resolved here.
    assert!(
        stdout.contains("${secret:GH_TOKEN}"),
        "secret ref missing:\n{stdout}"
    );
}

#[test]
fn config_show_raw_is_verbatim() {
    let tmp = TempDir::new().unwrap();
    let cfg = write_multi_file_workspace(tmp.path());

    let out = run(&cfg, &["config", "show", "--raw"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);

    // Raw = the root file untouched: include: is still there, fragment not merged.
    assert!(
        stdout.contains("include:"),
        "raw should keep include:\n{stdout}"
    );
    assert!(
        !stdout.contains("gh_team"),
        "raw must not assemble fragments:\n{stdout}"
    );
    assert_eq!(stdout, std::fs::read_to_string(&cfg).unwrap());
}

#[test]
fn config_show_tree_prints_include_graph() {
    let tmp = TempDir::new().unwrap();
    let cfg = write_multi_file_workspace(tmp.path());

    let out = run(&cfg, &["config", "show", "--tree"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        stdout.contains("pawrly.yaml"),
        "tree missing root:\n{stdout}"
    );
    assert!(
        stdout.contains("team.yaml"),
        "tree missing fragment:\n{stdout}"
    );
}

#[test]
fn source_list_annotates_origin_file() {
    let tmp = TempDir::new().unwrap();
    let glob = format!("{}/*.parquet", fixtures_dir().display());

    // Root declares `data`; the included fragment declares `more`.
    let root = tmp.path().join("pawrly.yaml");
    std::fs::write(
        &root,
        format!(
            "version: 1\ninclude:\n  - ./frag.yaml\nsources:\n  - name: data\n    kind: file\n    config:\n      path: {glob}\n"
        ),
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("frag.yaml"),
        format!("sources:\n  - name: more\n    kind: file\n    config:\n      path: {glob}\n"),
    )
    .unwrap();

    let out = run(&root, &["source", "list", "--json"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let parsed: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).unwrap();
    let arr = parsed.as_array().expect("array");

    let data = arr
        .iter()
        .find(|s| s["name"] == "data")
        .expect("data source");
    assert_eq!(data["origin"], "pawrly.yaml", "entry: {data}");
    let more = arr
        .iter()
        .find(|s| s["name"] == "more")
        .expect("more source");
    assert_eq!(more["origin"], "frag.yaml", "entry: {more}");
}
