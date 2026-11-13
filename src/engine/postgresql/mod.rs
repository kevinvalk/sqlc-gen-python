//! PostgreSQL: the Python type each of its column types maps to, and how its
//! lexer reads a statement.

mod lexer;
mod types;

pub use lexer::{is_identifier, merges, sql};
pub use types::{array_problem, is_guess, scalar_type};
