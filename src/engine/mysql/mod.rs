//! MySQL: the Python type each of its column types maps to, and how its lexer
//! reads a statement.

mod lexer;
mod types;

pub use lexer::{is_identifier, sql};
pub use types::{enum_problem, is_guess, scalar_type};

/// MySQL has no arrays.
pub fn array_problem(_data_type: &str) -> Option<&'static str> {
    Some("MySQL does not have")
}

/// MySQL has no `MERGE`.
pub fn merges(_sql: &str) -> bool {
    false
}
