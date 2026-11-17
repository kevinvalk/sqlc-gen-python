use ruff_python_ast as ast;
use ruff_python_ast::relocate::relocate_expr;
use ruff_python_parser::parse_expression;
use ruff_text_size::TextRange;
use serde::Deserialize;

use super::vocabulary::{self as names, Symbol, is_imported};
use super::{Expression, ellipsis, name, unparse};

#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    /// A name of the [vocabulary](super::vocabulary), or a union of several (`IPv4Network | IPv6Network`).
    Python(&'static [Symbol]),
    /// A builtin the driver returns as another type, so we call it to convert. SQLite has no boolean, for example, and
    /// returns a `BOOLEAN` as `0` or `1`.
    Converted(Symbol),
    /// An enum class we generate in the models module.
    Enum(String),
    /// A type an override names ([`Annotation`]). We hand the driver's value over unconverted, but a JSON column's is
    /// still bound as JSON.
    Custom { annotation: ast::Expr, json: Option<Json> },
    /// A value of the backend's own class, such as a range. On SQLAlchemy we load and bind it through its PostgreSQL
    /// `sql_type` (`postgresql.INT4RANGE`), so every driver's value is one class.
    Library {
        annotation: ast::Expr,
        sql_type: Option<&'static str>,
    },
    /// Decoded JSON is annotated `object`, as the decoder is configurable and nothing narrower holds under every
    /// configuration. We keep which JSON type it came from, as a backend may bind each through its own type.
    Json(Json),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Json {
    Json,
    Jsonb,
}

/// The module a type is written into, as only a query module has to qualify a generated class (`models.Status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Models,
    Queries,
}

impl Type {
    // Every element type the engines' type maps can produce for a column, each exactly once.
    pub const BOOL: Self = Self::Python(&[names::BOOL]);
    pub const BOOL_FROM_INT: Self = Self::Converted(names::BOOL);
    pub const BYTES: Self = Self::Python(&[names::BYTES]);
    pub const CIDR: Self = Self::Python(&[names::IPV4_NETWORK, names::IPV6_NETWORK]);
    pub const DATE: Self = Self::Python(&[names::DATE]);
    pub const DATETIME: Self = Self::Python(&[names::DATETIME]);
    pub const DECIMAL: Self = Self::Python(&[names::DECIMAL]);
    pub const FLOAT: Self = Self::Python(&[names::FLOAT]);
    // An address with a prefix comes back as an interface, not an address.
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

    /// An override of a JSON column still binds as its JSON type.
    pub fn json(&self) -> Option<Json> {
        match self {
            Self::Json(json) | Self::Custom { json: Some(json), .. } => Some(*json),
            _ => None,
        }
    }

    pub fn annotation(&self, scope: Scope) -> ast::Expr {
        match (self, scope) {
            (Self::Python(names), _) => (names.iter().copied().map(ast::Expr::from))
                .reduce(Expression::union)
                .expect("a type has a name"),
            (Self::Converted(builtin), _) => (*builtin).into(),
            (Self::Json(_), _) => names::OBJECT.into(),
            (Self::Custom { annotation, .. } | Self::Library { annotation, .. }, _) => annotation.clone(),
            (Self::Enum(class), Scope::Models) => name(class),
            (Self::Enum(class), Scope::Queries) => names::MODELS.attr(class),
        }
    }

    /// What a parameter of this type accepts, which for JSON is any document the serializer encodes but a `str`: one is
    /// stored as a JSON string, so a `json.dumps(...)` would be encoded twice. A `Mapping` admits a `TypedDict`, and
    /// only `list[Any]` admits a list of anything, as `list` is invariant. A `float` admits an `int` and a `bool`.
    pub fn parameter_annotation(&self) -> ast::Expr {
        match self {
            Self::Json(_) => [
                names::MAPPING.index_each(vec![names::STR.into(), names::OBJECT.into()]),
                names::LIST.index(names::ANY),
                names::TUPLE.index_each(vec![names::OBJECT.into(), ellipsis()]),
                names::FLOAT.into(),
            ]
            .into_iter()
            .reduce(Expression::union)
            .expect("a document has a type"),
            _ => self.annotation(Scope::Queries),
        }
    }
}

/// An override's `python_type`, the only Python we parse rather than build, with the modules its dotted names read from
/// (`app.types.Embedding` reads `app.types`). We only accept names, subscripts, `|` and `None`, and a bare name must be
/// a builtin or one of the [`TYPES`] generated code imports.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(try_from = "String")]
pub struct Annotation {
    pub expr: ast::Expr,
    pub modules: Vec<String>,
}

impl TryFrom<String> for Annotation {
    type Error = anyhow::Error;

    fn try_from(source: String) -> anyhow::Result<Self> {
        let parsed = parse_expression(&source).map_err(|error| anyhow::anyhow!("not a Python expression: {error}"));
        let mut modules = Vec::new();
        let checked = parsed.and_then(|parsed| {
            let expr = *parsed.into_syntax().body;
            imported(&expr, &mut modules).map(|()| expr)
        });
        let mut expr = checked.map_err(|error| anyhow::anyhow!("python_type \"{source}\": {error}"))?;
        // We place it nowhere in a file, like every node we build ourselves.
        relocate_expr(&mut expr, TextRange::default());
        Ok(Self { expr, modules })
    }
}

/// Collects in `modules` each module the annotation reads a name from, or says why generated code cannot hold it.
fn imported(annotation: &ast::Expr, modules: &mut Vec<String>) -> anyhow::Result<()> {
    match annotation {
        ast::Expr::Name(name) => {
            let id = name.id.as_str();
            if !ruff_python_stdlib::builtins::is_python_builtin(id, 11, false)
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
                module if module == names::DT.as_ref() => {}
                "typing" if attribute.attr.as_str() == names::ANY.as_ref() => {
                    anyhow::bail!("generated code imports `Any` from `typing` itself: write `Any`")
                }
                _ if is_imported(&root.id) => {
                    anyhow::bail!("`{}` is a name generated code binds", root.id)
                }
                // We know what the user meant here, so let's tell them.
                "datetime" => anyhow::bail!(
                    "generated code imports `datetime` as `dt`: write `dt.{}`",
                    attribute.attr
                ),
                _ if (CONVENTIONS.iter())
                    .any(|aliased| module == *aliased || module.starts_with(&format!("{aliased}."))) =>
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
        // Several arguments are a tuple, which is no type on its own, and neither is the `...` of `tuple[int, ...]`.
        ast::Expr::Subscript(subscript) => {
            imported(&subscript.value, modules)?;
            let arguments = match &*subscript.slice {
                ast::Expr::Tuple(tuple) => tuple.elts.as_slice(),
                argument => std::slice::from_ref(argument),
            };
            for argument in arguments {
                if !argument.is_ellipsis_literal_expr() {
                    imported(argument, modules)?;
                }
            }
        }
        ast::Expr::BinOp(union) if union.op == ast::Operator::BitOr => {
            imported(&union.left, modules)?;
            imported(&union.right, modules)?;
        }
        ast::Expr::NoneLiteral(_) => {}
        _ => anyhow::bail!("`{}` is no type", unparse(annotation)),
    }
    Ok(())
}

// `Any` too, as an override is the user's own choice, written in their sqlc.yaml for everyone to see, where an unknown
// type is only ever an error
const TYPES: &[Symbol] = &[
    names::ANY,
    names::DECIMAL,
    names::UUID,
    names::IPV4_ADDRESS,
    names::IPV4_INTERFACE,
    names::IPV4_NETWORK,
    names::IPV6_ADDRESS,
    names::IPV6_INTERFACE,
    names::IPV6_NETWORK,
];

/// The modules ruff's import conventions (ICN001) alias by default.
const CONVENTIONS: &[&str] = &[
    "altair",
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
