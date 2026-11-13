//! SQLite: the Python type each of its declared column types maps to, and
//! how its tokenizer reads a statement.

mod lexer;
mod types;

pub use lexer::{is_identifier, sql};
pub use types::{array_problem, is_guess, scalar_type};

/// SQLite has no `MERGE`.
pub fn merges(_sql: &str) -> bool {
    false
}
