//! Authentication configuration for the server.
//!
//! Only the structure is in place; the actual middleware
//! enforcement is not yet implemented.
//! For now, the only enforced rule is "non-loopback TCP requires Bearer".

/// Configured authentication mode.
#[derive(Clone, Default)]
pub enum AuthMode {
    /// No authentication. Allowed on UDS or loopback TCP only.
    #[default]
    None,
    /// Bearer token. Required for non-loopback TCP.
    Bearer { token: String },
}

/// Auth layer; not yet implemented.
#[derive(Clone)]
pub struct AuthLayer;
