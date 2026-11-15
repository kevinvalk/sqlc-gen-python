//! A generated file is ruff's tree from the first node to the last. We only parse Python in two places: an override's
//! annotation ([`Annotation`]), which is the user's, and a module [`render()`] has unparsed, to spell its strings and
//! for ruff to format.

mod imports;
mod render;
mod syntax;
mod types;
pub mod vocabulary;

pub use render::{render, trips_s105};
pub use syntax::*;
pub use types::{Annotation, Json, Scope, Type};
pub use vocabulary::is_imported;
