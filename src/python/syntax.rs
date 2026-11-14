//! The syntax a generated file is built from: the names it may bind, the
//! nodes that hold data, and the fragments this crate writes whole.

use ruff_python_ast::{self as ast, name::Name};
use ruff_python_codegen::{Generator, Indentation};
use ruff_python_parser::{parse_expression, parse_module};
use ruff_source_file::LineEnding;
use ruff_text_size::TextRange;
use thin_vec::ThinVec;

#[cfg(test)]
#[path = "syntax_spec.rs"]
mod spec;

/// A template written as a coroutine's (`async with`, `await`), as a
/// coroutine's, or a blocking function's: the same without `async ` and
/// `await `, which a template, holding no data, has nowhere else.
pub fn coroutine(template: &str, asynchronous: bool) -> String {
    match asynchronous {
        true => template.into(),
        false => template.replace("async ", "").replace("await ", ""),
    }
}

/// Parses an expression, such as `dataclasses.dataclass()`.
///
/// For fragments written in this crate only, or an override's annotation,
/// already one ([`super::annotation`]). Schema data goes into a template's holes.
pub fn expr(source: &str) -> ast::Expr {
    *parse_expression(source)
        .unwrap_or_else(|error| panic!("generated expression `{source}`: {error}"))
        .into_syntax()
        .body
}

/// Parses a statement, such as `cur.row_factory = None`.
///
/// Same caveat as [`expr`].
pub fn stmt(source: &str) -> ast::Stmt {
    (parse_module(source))
        .unwrap_or_else(|error| panic!("generated statement `{source}`: {error}"))
        .into_syntax()
        .body
        .pop()
        .expect("a statement")
}

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

/// A bare name, such as `int` or `row`.
pub fn name(id: &str) -> ast::Expr {
    node!(ast::ExprName {
        id: Name::new(id),
        ctx: ast::ExprContext::Load,
    })
    .into()
}

/// A keyword argument, such as `id=row[0]`.
pub fn keyword(name: &str, value: ast::Expr) -> ast::Keyword {
    node!(ast::Keyword {
        arg: Some(identifier(name)),
        value: value,
    })
}

/// A parameter without a default, such as `id: int`.
pub fn parameter(name: &str, annotation: ast::Expr) -> ast::ParameterWithDefault {
    node!(ast::ParameterWithDefault {
        parameter: node!(ast::Parameter {
            name: identifier(name),
            annotation: Some(Box::new(annotation)),
        }),
        default: None,
    })
}

/// An integer literal, such as an index.
pub fn int(value: usize) -> ast::Expr {
    node!(ast::ExprNumberLiteral {
        value: ast::Number::Int(ast::Int::from(value as u64)),
    })
    .into()
}

/// An expression statement, such as a call made for its effect.
pub fn expression(value: ast::Expr) -> ast::Stmt {
    node!(ast::StmtExpr {
        value: Box::new(value),
    })
    .into()
}

/// A tuple, such as a statement's values: `(id,)`, `(a, b)`.
pub fn tuple(items: Vec<ast::Expr>) -> ast::Expr {
    node!(ast::ExprTuple {
        elts: items,
        ctx: ast::ExprContext::Load,
        parenthesized: true,
    })
    .into()
}

/// `*value`: a list's items, each a value of its own.
pub fn starred(value: ast::Expr) -> ast::Expr {
    node!(ast::ExprStarred {
        value: Box::new(value),
        ctx: ast::ExprContext::Load,
    })
    .into()
}

/// A dict, such as a statement's named values: `{"id": id}`.
pub fn dict(items: Vec<(ast::Expr, ast::Expr)>) -> ast::Expr {
    node!(ast::ExprDict {
        items: (items.into_iter())
            .map(|(key, value)| ast::DictItem {
                key: Some(key),
                value,
            })
            .collect(),
    })
    .into()
}

/// A string literal.
pub fn string(value: &str) -> ast::Expr {
    node!(ast::ExprStringLiteral {
        value: ast::StringLiteralValue::single(node!(ast::StringLiteral {
            value: value.into(),
            flags: ast::StringLiteralFlags::empty().with_quote_style(ast::str::Quote::Double),
        })),
    })
    .into()
}

/// A docstring: first in a class or function body, or directly under the
/// attribute it documents. [`super::render()`] spells it.
///
/// This is how generated code carries documentation: ruff's AST has nowhere to
/// put a `#` comment, but a docstring is an ordinary string expression, and
/// tooling reads it where it would ignore a comment.
pub fn docstring(text: &str) -> ast::Stmt {
    node!(ast::StmtExpr {
        value: Box::new(string(text)),
    })
    .into()
}

/// An annotated declaration without a value, such as `id: int`.
pub fn ann_assign(target: &str, annotation: ast::Expr) -> ast::Stmt {
    node!(ast::StmtAnnAssign {
        target: Box::new(name(target)),
        annotation: Box::new(annotation),
        value: None,
        simple: true,
    })
    .into()
}

/// A constant, such as `_GET: Final = "SELECT 1"`.
pub fn constant(target: &str, value: ast::Expr) -> ast::Stmt {
    node!(ast::StmtAnnAssign {
        target: Box::new(name(target)),
        annotation: Box::new(name("Final")),
        value: Some(Box::new(value)),
        simple: true,
    })
    .into()
}

/// A plain assignment, such as `OPEN = "open"`.
pub fn assign(target: &str, value: ast::Expr) -> ast::Stmt {
    node!(ast::StmtAssign {
        targets: vec![name(target)],
        value: Box::new(value),
    })
    .into()
}

/// A class definition.
///
/// An empty base list omits the parentheses, and an empty body becomes `pass`,
/// since neither is valid Python written out literally.
pub fn class(
    name: &str,
    decorators: impl IntoIterator<Item = ast::Expr>,
    bases: impl IntoIterator<Item = ast::Expr>,
    body: impl IntoIterator<Item = ast::Stmt>,
) -> ast::Stmt {
    let bases: ThinVec<ast::Expr> = bases.into_iter().collect();
    let body: ThinVec<ast::Stmt> = body.into_iter().collect();

    node!(ast::StmtClassDef {
        decorator_list: decorators
            .into_iter()
            .map(|expression| node!(ast::Decorator {
                expression: expression
            }))
            .collect(),
        name: identifier(name),
        type_params: None,
        arguments: (!bases.is_empty()).then(|| {
            Box::new(node!(ast::Arguments {
                args: bases,
                keywords: Default::default(),
            }))
        }),
        body: match body.is_empty() {
            true => thin_vec::thin_vec![node!(ast::StmtPass {}).into()],
            false => body,
        },
    })
    .into()
}

fn identifier(name: &str) -> ast::Identifier {
    ast::Identifier::new(name, TextRange::default())
}

/// The source of statements, unformatted, as a spec compares them.
#[cfg(test)]
pub fn stmts_source(stmts: &[ast::Stmt]) -> String {
    (stmts.iter())
        .map(|stmt| Generator::new(&Indentation::default(), LineEnding::Lf).stmt(stmt))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The source of an expression, for an error.
pub fn python_source(expr: &ast::Expr) -> String {
    Generator::new(&Indentation::default(), LineEnding::Lf).expr(expr)
}
