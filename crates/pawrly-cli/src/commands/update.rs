//! `pawrly update` — upgrade the installed `pawrly` binary in place.
//!
//! Delegates to the canonical `install.sh` rather than reimplementing platform
//! detection, checksum verification, and the build-from-source fallback. The
//! installer is pointed at the directory of the running executable so the
//! binary is replaced where it already lives.

use std::path::Path;
use std::process::Stdio;

use clap::Args as ClapArgs;
use tokio::io::AsyncWriteExt;

/// Default GitHub repository to pull releases from. Mirrors `install.sh`.
const DEFAULT_REPO: &str = "CITGuru/pawrly";
const INSTALL_BRANCH: &str = "main";

#[derive(ClapArgs, Debug)]
pub struct Args {
    /// Install a specific release tag (e.g. v0.1.0) instead of the latest.
    #[arg(long = "version", value_name = "TAG")]
    pub tag: Option<String>,

    /// Only report whether a newer version is available; don't install anything.
    #[arg(long)]
    pub check: bool,

    /// Reinstall even when already on the target version.
    #[arg(long)]
    pub force: bool,
}

pub async fn run(args: Args) -> anyhow::Result<()> {
    let repo = std::env::var("PAWRLY_REPO").unwrap_or_else(|_| DEFAULT_REPO.to_string());
    let current = env!("CARGO_PKG_VERSION");

    let target = match args.tag.clone() {
        Some(tag) => tag,
        None => latest_version(&repo).await?,
    };
    let target_norm = target.trim_start_matches('v');

    if target_norm == current && !args.force {
        println!("pawrly is already up to date (v{current})");
        return Ok(());
    }

    if args.check {
        println!("update available: v{current} → {target}");
        println!("run `pawrly update` to install it");
        return Ok(());
    }

    // Install over the running binary, wherever it already lives on PATH.
    let exe = std::env::current_exe()?;
    let install_dir = exe.parent().ok_or_else(|| {
        anyhow::anyhow!("cannot determine install directory for {}", exe.display())
    })?;

    if target_norm == current {
        println!("reinstalling pawrly v{current}");
    } else {
        println!("updating pawrly v{current} → {target}");
    }

    run_installer(&repo, install_dir, args.tag.as_deref(), args.force).await?;
    Ok(())
}

/// Resolve the latest release tag from the GitHub API.
async fn latest_version(repo: &str) -> anyhow::Result<String> {
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let resp = reqwest::Client::new()
        .get(&url)
        .header(reqwest::header::USER_AGENT, "pawrly-cli")
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .await?
        .error_for_status()?;

    #[derive(serde::Deserialize)]
    struct Release {
        tag_name: String,
    }

    let release: Release = resp.json().await?;
    if release.tag_name.is_empty() {
        anyhow::bail!("could not determine the latest release from {url}");
    }
    Ok(release.tag_name)
}

/// Fetch the canonical installer and run it via `sh`, targeting `install_dir`.
async fn run_installer(
    repo: &str,
    install_dir: &Path,
    version: Option<&str>,
    force: bool,
) -> anyhow::Result<()> {
    let script_url =
        format!("https://raw.githubusercontent.com/{repo}/{INSTALL_BRANCH}/scripts/install.sh");
    let script = reqwest::Client::new()
        .get(&script_url)
        .header(reqwest::header::USER_AGENT, "pawrly-cli")
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;

    // The child inherits our environment, so user-set overrides like
    // PAWRLY_NO_VERIFY or PAWRLY_BUILD_FROM_SOURCE still apply.
    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-s")
        .env("PAWRLY_INSTALL_DIR", install_dir)
        .env("PAWRLY_REPO", repo)
        .stdin(Stdio::piped());
    if let Some(v) = version {
        cmd.env("PAWRLY_VERSION", v);
    }
    if force {
        cmd.env("PAWRLY_FORCE", "1");
    }

    let mut child = cmd.spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(script.as_bytes()).await?;
        stdin.shutdown().await?;
    }
    let status = child.wait().await?;
    if !status.success() {
        anyhow::bail!(
            "installer exited with {}",
            status
                .code()
                .map_or_else(|| "a signal".to_string(), |c| format!("status {c}"))
        );
    }
    Ok(())
}
