//! Acceptance: spawn `pawrly serve` on a UDS in a tempdir, then
//! run `pawrly --remote unix://<path> status` against it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::cargo::cargo_bin;
use tempfile::TempDir;

fn pawrly() -> Command {
    Command::new(cargo_bin("pawrly"))
}

#[test]
fn serve_then_status_round_trip() {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("pawrly.sock");
    let pid = tmp.path().join("pawrly.pid");

    // Spawn the daemon in the background.
    let mut daemon = pawrly()
        .args([
            "serve",
            "--addr",
            &format!("unix://{}", sock.display()),
            "--pid-file",
            pid.to_str().unwrap(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn daemon");

    // Wait up to 5s for the socket to appear.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !sock.exists() {
        if Instant::now() > deadline {
            let _ = daemon.kill();
            panic!("daemon never created socket at {}", sock.display());
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    // Run `pawrly status --endpoint unix://<sock>`.
    let out = pawrly()
        .args([
            "--no-remote", // required for top-level dispatch but ignored by `status`
            "status",
            "--endpoint",
            &format!("unix://{}", sock.display()),
        ])
        .output()
        .expect("status");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        out.status.success(),
        "status failed: stdout={stdout}\nstderr={stderr}"
    );
    assert!(
        stdout.contains("pawrly daemon running"),
        "unexpected stdout: {stdout}"
    );

    // Stop the daemon (the test owns the child).
    let _ = daemon.kill();
    let _ = daemon.wait();
}
