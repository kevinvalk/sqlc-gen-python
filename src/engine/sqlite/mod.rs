//! SQLite: the Python type each of its declared column types maps to, and
//! how its tokenizer reads a statement.

mod lexer;
mod types;

pub use lexer::{is_identifier, sql, words};
pub use types::scalar_type;
