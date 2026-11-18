//! We build every node of ruff's tree instead of parsing Python, so data (a column called `class`, SQL holding `"`) is
//! a name or a string and nothing else, and a construct that is not Python does not compile. A constructor whose name
//! is a Rust keyword takes PEP 8's trailing underscore (`if_`, `for_`, `return_`).

use ruff_python_ast as ast;
use ruff_python_codegen::{Generator, Indentation};
use ruff_source_file::LineEnding;
use ruff_text_size::TextRange;

/// Builds a ruff AST node with the position metadata that only parsed code has left at its default.
macro_rules! node {
    ($struct:path { $($field:ident : $value:expr),* $(,)? }) => {{
        $struct {
            $($field: $value,)*
            range: TextRange::default(),
            node_index: ast::AtomicNodeIndex::default(),
        }
    }};
}

mod expression;
mod statement;

#[cfg(test)]
#[path = "syntax_spec.rs"]
mod spec;

pub use expression::*;
pub use statement::*;

/// A public name is an identifier and no keyword. It must be ASCII, as Python NFKC-normalises identifiers (`ﬁle` is
/// `file`, so two spellings could bind one name) and ruff rejects the rest (PLC2401). It must not start with `_`, as
/// Python then reads it differently: the name is private, a class body mangles `__x`, pydantic leaves `_x` out of a
/// model's fields, and `enum` reserves `_sunder_` names.
pub fn is_public_name(name: &str) -> bool {
    name.is_ascii() && ruff_python_stdlib::identifiers::is_identifier(name) && !name.starts_with('_')
}

/// Whether `name` is a builtin of any Python ruff knows of, which a name generated code binds must not hide (ruff
/// A001): a function `list` would turn every `list[...]` of its module into an error. Ruff lists a builtin from the
/// Python that added it on, so we ask it of a version no Python has yet, and a ruff upgrade brings the next one's.
pub fn hides_builtin(name: &str) -> bool {
    ruff_python_stdlib::builtins::is_python_builtin(name, u8::MAX, false)
}

/// Whether `name` is a builtin of every Python, so generated code may read it bare: ruff's builtins of no particular
/// version, which `frozendict` (3.15) is not.
pub fn is_builtin(name: &str) -> bool {
    ruff_python_stdlib::builtins::is_python_builtin(name, 0, false)
}

/// A public CapWords name (ruff N801) that hides no builtin (`Warning`, `ExceptionGroup`).
pub fn is_class_name(name: &str) -> bool {
    is_public_name(name)
        && name.starts_with(|c: char| c.is_ascii_uppercase())
        && !name.contains('_')
        && !hides_builtin(name)
}

/// A public lowercase name (ruff N999), so no two modules are one file on a case-insensitive file system.
pub fn is_module_name(name: &str) -> bool {
    is_public_name(name) && !name.chars().any(|c| c.is_ascii_uppercase())
}

fn identifier(name: impl AsRef<str>) -> ast::Identifier {
    ast::Identifier::new(name.as_ref(), TextRange::default())
}

pub fn unparse(expr: &ast::Expr) -> String {
    Generator::new(&Indentation::default(), LineEnding::Lf).expr(expr)
}

pub fn unparse_stmt(stmt: &ast::Stmt) -> String {
    Generator::new(&Indentation::default(), LineEnding::Lf).stmt(stmt)
}

#[cfg(test)]
pub fn unparse_stmts(stmts: &[ast::Stmt]) -> String {
    let stmts: Vec<String> = stmts.iter().map(unparse_stmt).collect();
    stmts.join("\n")
}
