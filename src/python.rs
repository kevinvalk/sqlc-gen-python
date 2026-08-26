//! Writing Python.
//!
//! Three things live here, because they are one concern seen at three levels:
//! the [syntax](#syntax) a generated file is built from, the
//! [types](#types) it refers to, and the [imports](#imports) that make those
//! types resolve. A type reference and its import are held as a single value
//! rather than two fields, so `decimal.Decimal` and the `import decimal` that
//! gives it meaning cannot drift apart.
//!
//! Most of a generated file is fixed Python with data slotted into a few
//! places. For the fixed parts there is nothing to build: [`expr`] and [`stmt`]
//! hand the text to ruff's parser, so the source reads as the Python it
//! produces. Only where schema data lands is a node constructed by hand, and
//! only because a column name is not guaranteed to be a Python identifier.
//!
//! [`render`] closes the loop: ruff unparses the tree, then ruff formats it.

use std::collections::BTreeSet;

use anyhow::{Context, Result};
use ruff_python_ast::{self as ast, name::Name};
use ruff_python_codegen::{Generator, Indentation};
use ruff_python_formatter::{PyFormatOptions, format_module_source};
use ruff_python_parser::{parse_expression, parse_module};
use ruff_source_file::LineEnding;
use ruff_text_size::TextRange;
use thin_vec::ThinVec;

#[cfg(test)]
#[path = "python_spec.rs"]
mod spec;

// -- Rendering ---------------------------------------------------------------

/// Renders a module: ruff unparses the tree, ruff formats the result.
///
/// `Generator::generate` is `pub(crate)` to ruff, so there is no whole-module
/// entry point that hands back its buffer; each top-level statement is unparsed
/// on its own. Nothing is lost by that — the blank lines between them are the
/// formatter's decision either way.
pub fn render(module: &ast::ModModule) -> Result<String> {
    let unparsed = module
        .body
        .iter()
        .map(|stmt| Generator::new(&Indentation::default(), LineEnding::Lf).stmt(stmt))
        .collect::<Vec<_>>()
        .join("\n");

    let formatted = format_module_source(&unparsed, PyFormatOptions::default())
        .map_err(|error| anyhow::anyhow!("{error}"))
        .with_context(|| format!("formatting generated Python:\n{unparsed}"))?;

    Ok(formatted.into_code())
}

// -- Fragments ---------------------------------------------------------------

/// Parses an expression, such as `dataclasses.dataclass()`.
///
/// For fragments written in this crate only. Schema data goes through the
/// constructors below: a column may be called `class`, which is a fine SQL
/// identifier and a syntax error in Python.
pub fn expr(source: &str) -> ast::Expr {
    *parse_expression(source)
        .unwrap_or_else(|error| panic!("generated expression `{source}`: {error}"))
        .into_syntax()
        .body
}

/// Parses a statement, such as `from typing import Optional`.
///
/// Same caveat as [`expr`].
pub fn stmt(source: &str) -> ast::Stmt {
    parse_module(source)
        .unwrap_or_else(|error| panic!("generated statement `{source}`: {error}"))
        .into_syntax()
        .body
        .pop()
        .expect("a statement")
}

// -- Syntax ------------------------------------------------------------------

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

/// A module, the root of every generated file.
pub fn module(body: impl IntoIterator<Item = ast::Stmt>) -> ast::ModModule {
    node!(ast::ModModule {
        body: body.into_iter().collect(),
    })
}

/// A bare name, such as `int` or `row`.
pub fn name(id: &str) -> ast::Expr {
    node!(ast::ExprName {
        id: Name::new(id),
        ctx: ast::ExprContext::Load,
    })
    .into()
}

/// An attribute access, such as `models.Author`.
pub fn attribute(value: ast::Expr, attr: &str) -> ast::Expr {
    node!(ast::ExprAttribute {
        value: Box::new(value),
        attr: identifier(attr),
        ctx: ast::ExprContext::Load,
    })
    .into()
}

/// A subscript, such as `Optional[str]`.
pub fn subscript(value: ast::Expr, slice: ast::Expr) -> ast::Expr {
    node!(ast::ExprSubscript {
        value: Box::new(value),
        slice: Box::new(slice),
        ctx: ast::ExprContext::Load,
    })
    .into()
}

/// A string literal.
pub fn string(value: &str) -> ast::Expr {
    string_with(value, ast::str::TripleQuotes::No)
}

/// A docstring, as the first statement of a module, class or function body.
///
/// This is how generated code carries documentation: ruff's AST has nowhere to
/// put a `#` comment, but a docstring is an ordinary string expression, and
/// tooling reads it where it would ignore a comment.
pub fn docstring(text: &str) -> ast::Stmt {
    node!(ast::StmtExpr {
        value: Box::new(string_with(text, ast::str::TripleQuotes::Yes)),
    })
    .into()
}

/// An annotated assignment, such as `id: int` or `bio: Optional[str] = None`.
pub fn ann_assign(target: &str, annotation: ast::Expr, value: Option<ast::Expr>) -> ast::Stmt {
    node!(ast::StmtAnnAssign {
        target: Box::new(name(target)),
        annotation: Box::new(annotation),
        value: value.map(Box::new),
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
    let bases: Box<[ast::Expr]> = bases.into_iter().collect();
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

fn string_with(value: &str, triple_quotes: ast::str::TripleQuotes) -> ast::Expr {
    node!(ast::ExprStringLiteral {
        value: ast::StringLiteralValue::single(node!(ast::StringLiteral {
            value: value.into(),
            flags: ast::StringLiteralFlags::empty()
                .with_quote_style(ast::str::Quote::Double)
                .with_triple_quotes(triple_quotes),
        })),
    })
    .into()
}

fn identifier(name: &str) -> ast::Identifier {
    ast::Identifier::new(name, TextRange::default())
}

// -- Types -------------------------------------------------------------------

/// A reference to a Python type: what to write, and where it comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type {
    /// A builtin, needing no import: `int`.
    Builtin(&'static str),
    /// Reached through its module: `import decimal` makes `decimal.Decimal`.
    Qualified {
        module: &'static str,
        attribute: &'static str,
    },
    /// Reached by name: `from typing import Any` makes `Any`.
    Named {
        module: &'static str,
        name: &'static str,
    },
    /// A class this plugin generates, so there is nothing to import — though
    /// how it is written depends on which module is doing the writing.
    Generated(String),
}

/// Which module a type reference is being written into.
///
/// Generated classes live in the models module, so only code outside it has to
/// qualify them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope<'a> {
    /// The models module, where generated classes are already in scope.
    Models,
    /// A query module, which reaches them through the models module.
    Queries { models_module: &'a str },
}

impl Type {
    // The closed vocabulary the engine tables map onto: every Python type this
    // plugin can produce for a column appears here exactly once.
    pub const ANY: Self = Self::Named {
        module: "typing",
        name: "Any",
    };
    pub const BOOL: Self = Self::Builtin("bool");
    pub const BYTES: Self = Self::Builtin("bytes");
    pub const DATE: Self = Self::Qualified {
        module: "datetime",
        attribute: "date",
    };
    pub const DATETIME: Self = Self::Qualified {
        module: "datetime",
        attribute: "datetime",
    };
    pub const DECIMAL: Self = Self::Qualified {
        module: "decimal",
        attribute: "Decimal",
    };
    pub const FLOAT: Self = Self::Builtin("float");
    pub const INT: Self = Self::Builtin("int");
    pub const LIST: Self = Self::Named {
        module: "typing",
        name: "List",
    };
    pub const NONE: Self = Self::Builtin("None");
    pub const OPTIONAL: Self = Self::Named {
        module: "typing",
        name: "Optional",
    };
    pub const STR: Self = Self::Builtin("str");
    pub const TIME: Self = Self::Qualified {
        module: "datetime",
        attribute: "time",
    };
    pub const TIMEDELTA: Self = Self::Qualified {
        module: "datetime",
        attribute: "timedelta",
    };
    pub const UUID: Self = Self::Qualified {
        module: "uuid",
        attribute: "UUID",
    };

    /// The import that makes this type's name resolve, if it needs one.
    pub fn import(&self) -> Option<Import> {
        match self {
            Self::Builtin(_) | Self::Generated(_) => None,
            Self::Qualified { module, .. } => Some(Import::Module((*module).into())),
            Self::Named { module, name } => Some(Import::From((*module).into(), (*name).into())),
        }
    }

    /// The expression naming this type from within `scope`.
    pub fn expr(&self, scope: Scope<'_>) -> ast::Expr {
        match self {
            Self::Builtin(id) | Self::Named { name: id, .. } => name(id),
            // Both halves come from the tables above, so they can be written.
            Self::Qualified { module, attribute } => expr(&format!("{module}.{attribute}")),
            // The class name does not; it is derived from a table name.
            Self::Generated(class) => match scope {
                Scope::Models => name(class),
                Scope::Queries { models_module } => attribute(name(models_module), class),
            },
        }
    }
}

// -- Imports -----------------------------------------------------------------

/// A module a generated file depends on.
///
/// Owned rather than borrowed because not every import is known at compile
/// time: a query module imports its models from whatever package the user
/// configured.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Import {
    /// `import <module>`.
    Module(String),
    /// `from <module> import <name>`.
    From(String, String),
}

impl Import {
    /// `import <module>`.
    pub fn module(module: &str) -> Self {
        Self::Module(module.into())
    }

    /// `from <module> import <name>`.
    pub fn from(module: &str, name: &str) -> Self {
        Self::From(module.into(), name.into())
    }
}

/// The set of modules a generated file depends on.
///
/// Collected from the types a file actually mentions, so a type that stops
/// being used stops being imported without anyone having to remember.
#[derive(Debug, Default)]
pub struct Imports(BTreeSet<Import>);

impl FromIterator<Import> for Imports {
    fn from_iter<I: IntoIterator<Item = Import>>(imports: I) -> Self {
        Self(imports.into_iter().collect())
    }
}

impl Imports {
    /// The import statements, sorted, with names from the same module merged.
    ///
    /// Plain `import` statements come first, as isort orders them within a
    /// group. Grouping *between* stdlib and third party is left alone: it is
    /// expressed with blank lines, which an AST cannot carry.
    pub fn statements(&self) -> Vec<ast::Stmt> {
        let modules = self.0.iter().filter_map(|import| match import {
            Import::Module(module) => Some(stmt(&format!("import {module}"))),
            Import::From(..) => None,
        });

        // The set is sorted, so names from one module arrive consecutively.
        let mut merged: Vec<(&str, Vec<&str>)> = Vec::new();
        for import in &self.0 {
            if let Import::From(module, name) = import {
                match merged.last_mut() {
                    Some((last, names)) if last == module => names.push(name),
                    _ => merged.push((module, vec![name])),
                }
            }
        }

        let from_modules = (merged.into_iter())
            .map(|(module, names)| stmt(&format!("from {module} import {}", names.join(", "))));

        modules.chain(from_modules).collect()
    }
}
