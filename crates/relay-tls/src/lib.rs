//! Secure rustls listener configuration with atomic identity replacement.

mod acceptor;

pub use acceptor::{ReloadingTlsAcceptor, TlsLoadError};
