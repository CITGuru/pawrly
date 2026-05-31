//! Acceptance: `pawrly sql 'SELECT count(*) FROM data.orders'` works
//! against fixture parquet, both in local mode and through `pawrly serve`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

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

fn write_workspace(dir: &std::path::Path) -> PathBuf {
    let fx = fixtures_dir();
    let yaml = format!(
        r#"version: 1
sources:
  - name: data
    kind: file
    config:
      path: "{}"
    tables:
      - name: orders
        path: "{}"
        format: parquet
      - name: customers
        path: "{}"
        format: csv
"#,
        fx.display(),
        fx.join("orders.parquet").display(),
        fx.join("customers.csv").display(),
    );
    let path = dir.join("pawrly.yaml");
    std::fs::write(&path, yaml).unwrap();
    path
}

#[test]
fn local_sql_count_against_fixture_parquet() {
    let tmp = TempDir::new().unwrap();
    let cfg = write_workspace(tmp.path());
    let out = pawrly()
        .args([
            "--no-remote",
            "--config",
            cfg.to_str().unwrap(),
            "sql",
            "SELECT COUNT(*) AS n FROM data.orders",
            "--format",
            "json",
        ])
        .output()
        .expect("sql");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "non-zero exit: stderr={stderr}");
    assert!(
        stdout.contains("\"n\":\"5\""),
        "unexpected stdout: {stdout}"
    );
}

#[test]
fn local_schema_lists_tables() {
    let tmp = TempDir::new().unwrap();
    let cfg = write_workspace(tmp.path());
    let out = pawrly()
        .args(["--no-remote", "--config", cfg.to_str().unwrap(), "schema"])
        .output()
        .expect("schema");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success());
    assert!(
        stdout.contains("data.orders"),
        "missing data.orders: {stdout}"
    );
    assert!(
        stdout.contains("data.customers"),
        "missing data.customers: {stdout}"
    );
}

#[test]
fn local_sql_csv_format() {
    let tmp = TempDir::new().unwrap();
    let cfg = write_workspace(tmp.path());
    let out = pawrly()
        .args([
            "--no-remote",
            "--config",
            cfg.to_str().unwrap(),
            "sql",
            "SELECT id, name FROM data.customers ORDER BY id",
            "--format",
            "csv",
        ])
        .output()
        .expect("sql");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success());
    assert!(stdout.contains("id,name"), "missing header: {stdout}");
    assert!(stdout.contains("Acme Corp"), "missing row: {stdout}");
}

#[test]
fn daemon_mode_sql_round_trip() {
    let tmp = TempDir::new().unwrap();
    let cfg = write_workspace(tmp.path());
    let sock = tmp.path().join("pawrly.sock");

    // 1. Start the daemon with this config.
    let mut daemon = pawrly()
        .args([
            "--config",
            cfg.to_str().unwrap(),
            "serve",
            "--addr",
            &format!("unix://{}", sock.display()),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn daemon");

    let deadline = Instant::now() + Duration::from_secs(5);
    while !sock.exists() {
        if Instant::now() > deadline {
            let _ = daemon.kill();
            panic!("daemon never created socket at {}", sock.display());
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    // 2. Run the same SQL against the daemon via --remote.
    let out = pawrly()
        .args([
            "--remote",
            &format!("unix://{}", sock.display()),
            "sql",
            "SELECT COUNT(*) AS n FROM data.orders",
            "--format",
            "json",
        ])
        .output()
        .expect("remote sql");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    let _ = daemon.kill();
    let _ = daemon.wait();

    assert!(out.status.success(), "stderr={stderr}");
    assert!(stdout.contains("\"n\":\"5\""), "stdout={stdout}");
}

/// Acceptance: local (`--no-remote`) and daemon (`--remote`) modes must
/// produce identical stdout, byte-for-byte, for the same SQL across every
/// user-visible output format.
#[test]
fn local_and_daemon_byte_for_byte_parity() {
    let tmp = TempDir::new().unwrap();
    let cfg = write_workspace(tmp.path());
    let sock = tmp.path().join("pawrly.sock");

    let mut daemon = pawrly()
        .args([
            "--config",
            cfg.to_str().unwrap(),
            "serve",
            "--addr",
            &format!("unix://{}", sock.display()),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn daemon");

    let deadline = Instant::now() + Duration::from_secs(5);
    while !sock.exists() {
        if Instant::now() > deadline {
            let _ = daemon.kill();
            let _ = daemon.wait();
            panic!("daemon never created socket at {}", sock.display());
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    // Deterministic multi-row SQL: small, ordered, exercises multiple columns
    // and is unaffected by row arrival order from the engine.
    let sql = "SELECT id, name FROM data.customers ORDER BY id";
    let remote_addr = format!("unix://{}", sock.display());

    let mut failures: Vec<String> = Vec::new();
    for format in ["table", "json", "csv"] {
        let local = pawrly()
            .args([
                "--no-remote",
                "--config",
                cfg.to_str().unwrap(),
                "sql",
                sql,
                "--format",
                format,
            ])
            .output()
            .expect("local sql");
        let remote = pawrly()
            .args(["--remote", &remote_addr, "sql", sql, "--format", format])
            .output()
            .expect("remote sql");

        if !local.status.success() || !remote.status.success() {
            failures.push(format!(
                "format={format}: non-zero exit (local={} remote={})\n  local stderr={}\n  remote stderr={}",
                local.status,
                remote.status,
                String::from_utf8_lossy(&local.stderr),
                String::from_utf8_lossy(&remote.stderr),
            ));
            continue;
        }

        if local.stdout != remote.stdout {
            failures.push(format!(
                "format={format}: stdout differs\n  local ({} bytes)={:?}\n  remote ({} bytes)={:?}",
                local.stdout.len(),
                String::from_utf8_lossy(&local.stdout),
                remote.stdout.len(),
                String::from_utf8_lossy(&remote.stdout),
            ));
        }
    }

    let _ = daemon.kill();
    let _ = daemon.wait();

    assert!(failures.is_empty(), "{}", failures.join("\n---\n"));
}
