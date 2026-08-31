//! Request authentication without custom cryptographic primitives.

mod authenticator;

pub use authenticator::{AuthBuildError, AuthError, AuthIdentity, Authenticator};
