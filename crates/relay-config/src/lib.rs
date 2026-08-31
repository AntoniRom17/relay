//! Relay's configuration model, loader, and semantic validation.

mod model;
mod validation;

pub use model::*;
pub use validation::{ConfigError, load_from_path, parse, validate};
