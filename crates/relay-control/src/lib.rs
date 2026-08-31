//! Loopback-oriented health, metrics, and administration API.

mod server;

pub use server::{ControlPlane, ControlPlaneError};
