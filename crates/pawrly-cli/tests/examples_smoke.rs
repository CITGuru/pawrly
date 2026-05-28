//! Smoke tests for the configs under `examples/`. POWA-121.
//!
//! These guard the M3 promise that the shipped example configs work
//! out of the box. If a contributor adds a new file under `examples/`,
//! they should also add a smoke check here.

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
