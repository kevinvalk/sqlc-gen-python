//! We build generated files as ruff trees. We parse override expressions from the configuration, quoted cast types
//! to collect their imports, and the module [`render()`] unparsed so ruff can format it.

mod imports;
mod render;
mod syntax;
mod types;
pub mod vocabulary;

pub use render::{render, trips_s105};
pub use syntax::*;
pub use types::{Annotation, Converters, Json, Scope, Type};
pub use vocabulary::is_imported;

/// The minor version of the oldest Python 3 we generate for, `requires-python` in `tests/python/pyproject.toml`.
pub const OLDEST_MINOR: u8 = 11;
