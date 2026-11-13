//! MySQL: the Python type each of its column types maps to, and how its lexer
//! reads a statement.

mod lexer;
mod types;

pub use lexer::{is_identifier, sql, words};
pub use types::{enum_problem, scalar_type};
