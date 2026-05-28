//! Tries each backend in order; first hit wins.

use secrecy::SecretString;

use crate::{SecretError, SecretStore};

/// A composite store that consults each backend in order.
///
/// `get(name)` returns the first `Ok(Some(_))`. Backend errors are propagated
/// immediately (no fallthrough on hard errors), so configuration mistakes
/// are loud.
pub struct LayeredStore {
    backends: Vec<Box<dyn SecretStore>>,
}

impl LayeredStore {
    /// Construct from an ordered list of backends.
    #[must_use]
    pub fn new(backends: Vec<Box<dyn SecretStore>>) -> Self {
        Self { backends }
    }

    /// Number of configured backends.
    #[must_use]
    pub fn len(&self) -> usize {
        self.backends.len()
    }

    /// True if no backends are configured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.backends.is_empty()
    }
}

impl SecretStore for LayeredStore {
    fn get(&self, name: &str) -> Result<Option<SecretString>, SecretError> {
        for backend in &self.backends {
            match backend.get(name)? {
                Some(s) => return Ok(Some(s)),
                None => continue,
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StaticStore;
    use secrecy::ExposeSecret as _;

    #[test]
    fn first_hit_wins() {
        let a = StaticStore::new();
        a.insert("X", "from-a");
        let b = StaticStore::new();
        b.insert("X", "from-b");
        b.insert("Y", "y-from-b");

        let layered = LayeredStore::new(vec![Box::new(a), Box::new(b)]);
        assert_eq!(layered.get("X").unwrap().unwrap().expose_secret(), "from-a");
        assert_eq!(
            layered.get("Y").unwrap().unwrap().expose_secret(),
            "y-from-b"
        );
        assert!(layered.get("Z").unwrap().is_none());
    }

    #[test]
    fn empty_layered() {
        let layered = LayeredStore::new(Vec::new());
        assert!(layered.is_empty());
        assert!(layered.get("X").unwrap().is_none());
    }
}
