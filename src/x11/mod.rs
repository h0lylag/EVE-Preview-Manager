//! X11 u window detection.

mod context;
mod ops;
mod query;

pub use context::{AppContext, CachedAtoms, CachedFormats, to_fixed};
pub use ops::*;
pub use query::*;
