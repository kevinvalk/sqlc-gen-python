//! The syntax a generated file is built from, as ruff's tree: the names it
//! may bind, and a constructor for each expression and statement generated
//! code is made of. Nothing here parses Python: every node is built, so data
//! (a column called `class`, SQL holding `"`) is a name or a string and
//! nothing else, and a construct that is not Python does not compile.
//!
//! An expression grows as Python reads, through [`Expression`]:
//! `CUR.attr("execute").call([statement])` is `cur.execute(statement)`. A
//! constructor is named for what it builds; one that would be a Rust keyword
//! takes PEP 8's trailing underscore (`if_`, `for_`, `return_`).

use ruff_python_ast as ast;
use ruff_python_codegen::{Generator, Indentation};
use ruff_source_file::LineEnding;
use ruff_text_size::TextRange;

/// Builds a ruff AST node, defaulting the position metadata that only parsed
/// code populates.
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

/// Whether `name` can be bound as a public Python name.
///
/// That is a valid identifier and not a keyword, which the parser would reject;
/// ASCII, since Python NFKC-normalises identifiers (`ﬁle` is `file`, so two
/// distinct spellings could bind one name) and ruff rejects the rest
/// (PLC2401); and not `_`-prefixed, which Python then misreads: the name is
/// private, a class body mangles `__x`, pydantic leaves `_x` out of a model's
/// fields, and `enum` reserves `_sunder_` names.
pub fn is_public_name(name: &str) -> bool {
    name.is_ascii()
        && ruff_python_stdlib::identifiers::is_identifier(name)
        && !name.starts_with('_')
}

/// A name a generated class can take: a public CapWords name (ruff N801),
/// which hides no builtin of any supported Python (`Warning`,
/// `ExceptionGroup`) and is no keyword (`None`).
pub fn is_class_name(name: &str) -> bool {
    is_public_name(name)
        && name.starts_with(|c: char| c.is_ascii_uppercase())
        && !name.contains('_')
        && !ruff_python_stdlib::keyword::is_keyword(name)
        && !(10..=14)
            .any(|minor| ruff_python_stdlib::builtins::is_python_builtin(name, minor, false))
}

/// A name a module or package can be imported by: a public name in lowercase
/// (ruff N999), so that no two are one file on a case-insensitive file system.
pub fn is_module_name(name: &str) -> bool {
    is_public_name(name) && !name.chars().any(|c| c.is_ascii_uppercase())
}

fn identifier(name: impl AsRef<str>) -> ast::Identifier {
    ast::Identifier::new(name.as_ref(), TextRange::default())
}

/// `expr` as source: for a forward reference, or an error to show.
pub fn unparse(expr: &ast::Expr) -> String {
    Generator::new(&Indentation::default(), LineEnding::Lf).expr(expr)
}

/// `stmt` as source, unformatted: what [`super::render()`] hands the
/// formatter.
pub fn unparse_stmt(stmt: &ast::Stmt) -> String {
    Generator::new(&Indentation::default(), LineEnding::Lf).stmt(stmt)
}

/// `stmts` as source, a statement after another, as a spec compares them.
#[cfg(test)]
pub fn unparse_stmts(stmts: &[ast::Stmt]) -> String {
    let stmts: Vec<String> = stmts.iter().map(unparse_stmt).collect();
    stmts.join("\n")
}
