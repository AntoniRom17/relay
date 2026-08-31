//! Allocation-conscious request matching with deterministic precedence.

mod table;

pub use table::{RouteBuildError, RouteMatch, RouteTable};
