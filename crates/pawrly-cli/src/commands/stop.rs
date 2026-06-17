//! `pawrly stop` — signal a running daemon.

use std::path::PathBuf;

use clap::Args as ClapArgs;

#[derive(ClapArgs, Debug)]
pub struct Args {
    /// Path to the daemon's PID file. Default: `$PAWRLY_HOME/sockets/pawrly.pid`.
    #[arg(long)]
    pub pid_file: Option<PathBuf>,

    /// Force SIGKILL after a short grace period.
    #[arg(long)]
    pub force: bool,
}

pub async fn run(home: Option<PathBuf>, args: Args) -> anyhow::Result<()> {
    let sockets_dir = home
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|h| h.join(".pawrly"))
        })
        .unwrap_or_else(|| PathBuf::from("."))
        .join("sockets");
    let pid_file = args
        .pid_file
        .unwrap_or_else(|| sockets_dir.join("pawrly.pid"));

    let pid_str = std::fs::read_to_string(&pid_file)
        .map_err(|e| anyhow::anyhow!("could not read pid file `{}`: {e}", pid_file.display()))?;
    let pid: i32 = pid_str
        .trim()
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid pid `{}`: {e}", pid_str.trim()))?;

    #[cfg(unix)]
    {
        let signal = if args.force { 9 } else { 15 };
        // SAFETY: kill() is FFI but is safe given a valid pid; we accept failure here
        // by returning the OS error code via Result.
        #[allow(
            unsafe_code,
            reason = "POSIX kill(2) requires unsafe; we surface errors"
        )]
        let res = unsafe { libc::kill(pid, signal) };
        if res != 0 {
            return Err(anyhow::anyhow!(
                "kill({pid}, {signal}) failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        println!("sent signal {signal} to pid {pid}");

        // On SIGKILL the daemon can't unlink its own socket/pid, so clean up
        // the default paths here as a backstop. On a graceful SIGTERM the
        // daemon removes these itself; these best-effort removes are idempotent.
        if args.force {
            let _ = std::fs::remove_file(sockets_dir.join("pawrly.sock"));
            let _ = std::fs::remove_file(&pid_file);
        }
    }
    #[cfg(not(unix))]
    {
        anyhow::bail!("`pawrly stop` is only supported on Unix");
    }

    Ok(())
}
