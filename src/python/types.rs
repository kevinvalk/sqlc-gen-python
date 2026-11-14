//! The Python types a generated file refers to, and how each is written
//! from the module that names it.

use ruff_python_ast as ast;
use ruff_python_parser::parse_expression;

use super::imports::is_imported;
use super::{expr, name, py_expr, python_source};

/// A reference to a Python type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type {
    /// An annotation built from builtins and the [vocabulary](super::imports), as
    /// written: `int`, `dt.date`, `IPv4Network | IPv6Network`.
    Python(&'static str),
    /// A builtin a driver returns as another type, which calling it converts:
    /// SQLite has no boolean, and returns a `BOOLEAN` as `0` or `1`.
    Converted(&'static str),
    /// An enum class this plugin generates in the models module. How it is
    /// written depends on which module is doing the writing.
    Enum(String),
    /// A type an override names, written as given. A driver returns and
    /// takes the value unconverted; a JSON column's is still bound as JSON.
    Custom {
        annotation: String,
        json: Option<Json>,
    },
    /// A value of the backend's own class, such as a range: written as the
    /// backend names it, and on SQLAlchemy loaded and bound through the SQL
    /// type `sql_type` names, so that every driver's value is one class.
    Library {
        annotation: String,
        sql_type: Option<&'static str>,
    },
    /// Decoded JSON, `object`: the decoder is configurable, so nothing
    /// narrower holds under every configuration. Which JSON type it was
    /// decoded from is kept, as a backend may bind each through its own type.
    Json(Json),
}

/// A JSON column type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Json {
    Json,
    Jsonb,
}

/// Which module a type reference is being written into.
///
/// Generated classes live in the models module, so only code outside it has to
/// qualify them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// The models module, where generated classes are already in scope.
    Models,
    /// A query module, which reaches them through `from . import models`.
    Queries,
}

impl Type {
    // The closed vocabulary the type map (`engine/postgresql/types.rs`) maps
    // onto: every element type this plugin can produce for a column appears
    // here exactly once.
    pub const BOOL: Self = Self::Python("bool");
    pub const BOOL_FROM_INT: Self = Self::Converted("bool");
    pub const BYTES: Self = Self::Python("bytes");
    pub const CIDR: Self = Self::Python("IPv4Network | IPv6Network");
    pub const DATE: Self = Self::Python("dt.date");
    pub const DATETIME: Self = Self::Python("dt.datetime");
    pub const DECIMAL: Self = Self::Python("Decimal");
    pub const FLOAT: Self = Self::Python("float");
    /// An address with a prefix comes back as an interface, not an address.
    pub const INET: Self =
        Self::Python("IPv4Address | IPv6Address | IPv4Interface | IPv6Interface");
    pub const INT: Self = Self::Python("int");
    pub const JSON: Self = Self::Json(Json::Json);
    pub const JSONB: Self = Self::Json(Json::Jsonb);
    pub const STR: Self = Self::Python("str");
    pub const TIME: Self = Self::Python("dt.time");
    pub const TIMEDELTA: Self = Self::Python("dt.timedelta");
    pub const UUID: Self = Self::Python("UUID");

    /// The JSON type a value of this type binds as, if any: an override of a
    /// JSON column keeps it.
    pub fn json(&self) -> Option<Json> {
        match self {
            Self::Json(json)
            | Self::Custom {
                json: Some(json), ..
            } => Some(*json),
            _ => None,
        }
    }

    /// The annotation naming this type from within `scope`: one this crate
    /// writes, or an override's, which [`annotation`] made sure is one; and
    /// an enum's class, by name.
    pub fn annotation(&self, scope: Scope) -> ast::Expr {
        match (self, scope) {
            (Self::Python(source) | Self::Converted(source), _) => expr(source),
            (Self::Json(_), _) => name("object"),
            (Self::Custom { annotation, .. } | Self::Library { annotation, .. }, _) => {
                expr(annotation)
            }
            (Self::Enum(class), Scope::Models) => name(class),
            (Self::Enum(class), Scope::Queries) => py_expr!("models.$class", class = class),
        }
    }
}

/// An override's `annotation`, as ruff writes it, and the modules it reads
/// names from, to import: of each dotted name, all but its last part
/// (`app.types.Embedding` reads `app.types`). A bare name must be a builtin
/// or one of the [`TYPES`] generated code imports, and the annotation no more
/// than names, subscripts, `|` and `None`.
pub fn annotation(source: &str) -> anyhow::Result<(String, Vec<String>)> {
    fn walk(expr: &ast::Expr, modules: &mut Vec<String>) -> anyhow::Result<()> {
        match expr {
            ast::Expr::Name(name) => {
                let id = name.id.as_str();
                if !ruff_python_stdlib::builtins::is_python_builtin(id, 10, false)
                    && !TYPES.contains(&id)
                {
                    anyhow::bail!("`{id}` is neither a builtin nor a dotted name to import");
                }
            }
            ast::Expr::Attribute(attribute) => {
                let mut parts = vec![attribute.attr.as_str()];
                let mut value = &*attribute.value;
                while let ast::Expr::Attribute(inner) = value {
                    parts.push(inner.attr.as_str());
                    value = &inner.value;
                }
                let ast::Expr::Name(root) = value else {
                    anyhow::bail!("`{}` is no dotted name", python_source(expr));
                };
                parts.push(root.id.as_str());
                parts.reverse();
                parts.pop();
                let module = parts.join(".");
                match module.as_str() {
                    // `datetime`, as generated code imports it.
                    "dt" => {}
                    _ if is_imported(&root.id) => {
                        anyhow::bail!("`{}` is a name generated code binds", root.id)
                    }
                    _ if (CONVENTIONS.iter()).any(|aliased| {
                        module == *aliased || module.starts_with(&format!("{aliased}."))
                    }) =>
                    {
                        anyhow::bail!(
                            "ruff's import conventions (ICN001) alias `{module}`, which generated \
                             code imports bare; name the type through a module of your own"
                        )
                    }
                    _ if !modules.contains(&module) => modules.push(module),
                    _ => {}
                }
            }
            ast::Expr::Subscript(subscript) => {
                walk(&subscript.value, modules)?;
                walk(&subscript.slice, modules)?;
            }
            ast::Expr::Tuple(tuple) => {
                for item in &tuple.elts {
                    walk(item, modules)?;
                }
            }
            ast::Expr::BinOp(op) if op.op == ast::Operator::BitOr => {
                walk(&op.left, modules)?;
                walk(&op.right, modules)?;
            }
            ast::Expr::NoneLiteral(_) => {}
            _ => anyhow::bail!("`{}` is no type", python_source(expr)),
        }
        Ok(())
    }
    let expr = parse_expression(source)
        .map_err(|error| anyhow::anyhow!("not a Python expression: {error}"))?
        .into_syntax()
        .body;
    let mut modules = Vec::new();
    walk(&expr, &mut modules)?;
    Ok((python_source(&expr), modules))
}

/// The names generated code imports that an override may use bare.
const TYPES: &[&str] = &[
    "Decimal",
    "UUID",
    "IPv4Address",
    "IPv4Interface",
    "IPv4Network",
    "IPv6Address",
    "IPv6Interface",
    "IPv6Network",
];

/// The modules ruff's import conventions (ICN001) alias by default, and
/// `datetime`, which generated code imports as `dt`.
const CONVENTIONS: &[&str] = &[
    "altair",
    "datetime",
    "holoviews",
    "matplotlib",
    "networkx",
    "numpy",
    "pandas",
    "panel",
    "plotly.express",
    "polars",
    "pyarrow",
    "seaborn",
    "tensorflow",
    "tkinter",
    "xml.etree.ElementTree",
];
