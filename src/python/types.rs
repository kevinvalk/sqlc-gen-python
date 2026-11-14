//! The Python types a generated file refers to, and how each is written
//! from the module that names it.

use ruff_python_ast as ast;
use ruff_python_ast::relocate::relocate_expr;
use ruff_python_parser::parse_expression;
use ruff_text_size::TextRange;
use serde::Deserialize;

use super::vocabulary::{self as names, Symbol, is_imported};
use super::{Expression, name, unparse};

/// A reference to a Python type.
#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    /// A name of the [vocabulary](super::vocabulary), or a union of several:
    /// `int`, `dt.date`, `IPv4Network | IPv6Network`.
    Python(&'static [Symbol]),
    /// A builtin a driver returns as another type, which calling it converts:
    /// SQLite has no boolean, and returns a `BOOLEAN` as `0` or `1`.
    Converted(Symbol),
    /// An enum class this plugin generates in the models module. How it is
    /// written depends on which module is doing the writing.
    Enum(String),
    /// A type an override names ([`Annotation`]). A driver returns and takes
    /// the value unconverted; a JSON column's is still bound as JSON.
    Custom {
        annotation: ast::Expr,
        json: Option<Json>,
    },
    /// A value of the backend's own class, such as a range: annotated as the
    /// backend names it, and on SQLAlchemy loaded and bound through its
    /// PostgreSQL type `sql_type` (`postgresql.INT4RANGE`), so that every
    /// driver's value is one class.
    Library {
        annotation: ast::Expr,
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
    pub const BOOL: Self = Self::Python(&[names::BOOL]);
    pub const BOOL_FROM_INT: Self = Self::Converted(names::BOOL);
    pub const BYTES: Self = Self::Python(&[names::BYTES]);
    pub const CIDR: Self = Self::Python(&[names::IPV4_NETWORK, names::IPV6_NETWORK]);
    pub const DATE: Self = Self::Python(&[names::DATE]);
    pub const DATETIME: Self = Self::Python(&[names::DATETIME]);
    pub const DECIMAL: Self = Self::Python(&[names::DECIMAL]);
    pub const FLOAT: Self = Self::Python(&[names::FLOAT]);
    /// An address with a prefix comes back as an interface, not an address.
    pub const INET: Self = Self::Python(&[
        names::IPV4_ADDRESS,
        names::IPV6_ADDRESS,
        names::IPV4_INTERFACE,
        names::IPV6_INTERFACE,
    ]);
    pub const INT: Self = Self::Python(&[names::INT]);
    pub const JSON: Self = Self::Json(Json::Json);
    pub const JSONB: Self = Self::Json(Json::Jsonb);
    pub const STR: Self = Self::Python(&[names::STR]);
    pub const TIME: Self = Self::Python(&[names::TIME]);
    pub const TIMEDELTA: Self = Self::Python(&[names::TIMEDELTA]);
    pub const UUID: Self = Self::Python(&[names::UUID]);

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

    /// The annotation naming this type from within `scope`.
    pub fn annotation(&self, scope: Scope) -> ast::Expr {
        match (self, scope) {
            (Self::Python(names), _) => (names.iter().copied().map(ast::Expr::from))
                .reduce(Expression::union)
                .expect("a type has a name"),
            (Self::Converted(builtin), _) => (*builtin).into(),
            (Self::Json(_), _) => names::OBJECT.into(),
            (Self::Custom { annotation, .. } | Self::Library { annotation, .. }, _) => {
                annotation.clone()
            }
            (Self::Enum(class), Scope::Models) => name(class),
            (Self::Enum(class), Scope::Queries) => names::MODELS.attr(class),
        }
    }
}

/// An override's `python_type`: the only Python this crate reads rather than
/// builds. It is the annotation as a tree, and the modules it reads names
/// from, to import: of each dotted name, all but its last part
/// (`app.types.Embedding` reads `app.types`). A bare name must be a builtin
/// or one of the [`TYPES`] generated code imports, and the annotation no more
/// than names, subscripts, `|` and `None`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(try_from = "String")]
pub struct Annotation {
    pub expr: ast::Expr,
    pub modules: Vec<String>,
}

impl TryFrom<String> for Annotation {
    type Error = anyhow::Error;

    fn try_from(source: String) -> anyhow::Result<Self> {
        let parsed = parse_expression(&source)
            .map_err(|error| anyhow::anyhow!("not a Python expression: {error}"));
        let mut modules = Vec::new();
        let checked = parsed.and_then(|parsed| {
            let expr = *parsed.into_syntax().body;
            imported(&expr, &mut modules).map(|()| expr)
        });
        let mut expr =
            checked.map_err(|error| anyhow::anyhow!("python_type \"{source}\": {error}"))?;
        // Nowhere in a file yet, as every node this crate builds.
        relocate_expr(&mut expr, TextRange::default());
        Ok(Self { expr, modules })
    }
}

/// Notes in `modules` each module `annotation` reads a name from, or says why
/// it is no annotation generated code can hold.
fn imported(annotation: &ast::Expr, modules: &mut Vec<String>) -> anyhow::Result<()> {
    match annotation {
        ast::Expr::Name(name) => {
            let id = name.id.as_str();
            if !ruff_python_stdlib::builtins::is_python_builtin(id, 10, false)
                && !TYPES.iter().any(|ty| ty.as_ref() == id)
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
                anyhow::bail!("`{}` is no dotted name", unparse(annotation));
            };
            parts.push(root.id.as_str());
            parts.reverse();
            parts.pop();
            let module = parts.join(".");
            match module.as_str() {
                // `datetime`, as generated code imports it.
                module if module == names::DT.as_ref() => {}
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
            imported(&subscript.value, modules)?;
            imported(&subscript.slice, modules)?;
        }
        ast::Expr::Tuple(tuple) => {
            for item in &tuple.elts {
                imported(item, modules)?;
            }
        }
        ast::Expr::BinOp(op) if op.op == ast::Operator::BitOr => {
            imported(&op.left, modules)?;
            imported(&op.right, modules)?;
        }
        ast::Expr::NoneLiteral(_) => {}
        _ => anyhow::bail!("`{}` is no type", unparse(annotation)),
    }
    Ok(())
}

/// The names generated code imports that an override may use bare.
const TYPES: &[Symbol] = &[
    names::DECIMAL,
    names::UUID,
    names::IPV4_ADDRESS,
    names::IPV4_INTERFACE,
    names::IPV4_NETWORK,
    names::IPV6_ADDRESS,
    names::IPV6_INTERFACE,
    names::IPV6_NETWORK,
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
