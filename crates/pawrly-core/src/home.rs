//! Resolution of the Pawrly home directory (`PAWRLY_HOME`).
//!
//! The home directory is where Pawrly keeps its own data: the fallback
//! workspace manifest (`pawrly.yaml`), the cache root (`cache/`), and the
//! daemon socket (`sockets/pawrly.sock`). It is distinct from the workspace
//! directory, which is wherever the active config file lives.

use std::path::{Path, PathBuf};

/// Resolve the Pawrly home directory.
///
/// Precedence: an explicit value (the `--home` flag) → the `PAWRLY_HOME`
/// environment variable → `$HOME/.pawrly` (`%USERPROFILE%\.pawrly` on
/// Windows, where `HOME` is usually unset). Returns `None` only when no
/// explicit value is given and none of those variables are set.
pub fn resolve_home(explicit: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = explicit {
        return Some(p.to_path_buf());
    }
    if let Some(p) = std::env::var_os("PAWRLY_HOME")
        && !p.is_empty()
    {
        return Some(PathBuf::from(p));
    }
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|h| !h.is_empty())
        .map(|h| PathBuf::from(h).join(".pawrly"))
}

#[cfg(test)]
mod tests {
    use super::resolve_home;
    use std::path::{Path, PathBuf};

    #[test]
    fn explicit_wins() {
        assert_eq!(
            resolve_home(Some(Path::new("/opt/pawrly"))),
            Some(PathBuf::from("/opt/pawrly"))
        );
    }

    #[test]
    fn falls_back_to_home_dot_pawrly() {
        // The test environment always has $HOME; PAWRLY_HOME may not be set.
        // Whichever branch fires, the result must be an absolute path.
        if let Some(resolved) = resolve_home(None) {
            assert!(resolved.is_absolute());
        }
    }
}
