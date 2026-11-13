//! Writing Python.
//!
//! Three things live here, because they are one concern seen at three levels:
//! the [syntax](#syntax) a generated file is built from, the
//! [types](#types) it refers to, and the [imports](#imports) that make those
//! types resolve. Imports are never declared: [`render`] reads them off the
//! names its body reads, so a name and the import that gives it meaning
//! cannot drift apart.
//!
//! Most of a generated file is fixed Python with data slotted into a few
//! places. It is written as Python templates ([`py_expr!`], [`py_stmt!`],
//! [`py_stmts!`], see [`quote`]) whose holes take the data as nodes, built by
//! the constructors below: data never passes through the parser, so a column
//! called `class`, or SQL holding `"`, is a name or a string and nothing else.
//! [`expr`] and [`stmt`] parse a fragment this crate writes whole.
//!
//! [`render`] closes the loop: ruff unparses the tree, then ruff formats it.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use ruff_python_ast::visitor::{Visitor, walk_expr, walk_stmt};
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

pub mod quote;
pub use quote::Hole;

/// An expression from a template ([`quote`]): `py_expr!("$a + 1", a = x)`.
macro_rules! py_expr {
    ($template:expr $(, $hole:ident = $value:expr)* $(,)?) => {
        $crate::python::quote::expr(
            $template,
            vec![$((stringify!($hole), $crate::python::Hole::from($value))),*],
        )
    };
}

/// The one statement of a template ([`quote`]).
macro_rules! py_stmt {
    ($template:expr $(, $hole:ident = $value:expr)* $(,)?) => {{
        let mut stmts = $crate::python::py_stmts!($template $(, $hole = $value)*);
        assert_eq!(stmts.len(), 1, "template `{}` is one statement", $template);
        stmts.remove(0)
    }};
}

/// The statements of a template ([`quote`]).
macro_rules! py_stmts {
    ($template:expr $(, $hole:ident = $value:expr)* $(,)?) => {
        $crate::python::quote::stmts(
            $template,
            vec![$((stringify!($hole), $crate::python::Hole::from($value))),*],
        )
    };
}

pub(crate) use {py_expr, py_stmt, py_stmts};

// -- Rendering ---------------------------------------------------------------

/// Renders a module of `body`, after the imports it reads, of the
/// [vocabulary](VOCABULARY) and of `modules` (an override's): ruff unparses
/// the tree, ruff formats the result.
///
/// `Generator::generate` is `pub(crate)` to ruff, so there is no whole-module
/// entry point that hands back its buffer; each top-level statement is unparsed
/// on its own. Nothing is lost by that — the blank lines between them are the
/// formatter's decision either way.
///
/// The unparsed text is parsed back to find its multi-line strings, which fails
/// exactly when the tree was not valid Python. That is a bug in this crate,
/// never a property of the user's schema — unusable SQL names are handled
/// where names are made — so it panics, like a bad [`expr`] fragment does.
pub fn render(body: &[ast::Stmt], modules: &[String]) -> String {
    let mut unparsed = imports(body, modules);
    for stmt in body {
        unparsed += &Generator::new(&Indentation::default(), LineEnding::Lf).stmt(stmt);
        unparsed.push('\n');
    }
    format_module_source(&respell_strings(&unparsed), PyFormatOptions::default())
        .expect("respelt strings are valid Python")
        .into_code()
}

/// Whether ruff's S105, which takes a string assigned to a name like
/// `PASSWORD` or `_GET_API_TOKEN` for a hardcoded secret, flags anything in
/// `body`: what generated code assigns is SQL and enum labels, never a
/// secret, so its module exempts itself from the rule, and only then.
pub fn trips_s105(body: &[ast::Stmt]) -> bool {
    // ruff's own pattern (`flake8_bandit::helpers`).
    static SECRET: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"(^|_)(?i)(pas+wo?r?d|pass(phrase)?|pwd|token|secrete?)($|_)")
            .expect("ruff's pattern")
    });
    body.iter().any(|stmt| {
        let (targets, value): (Vec<&ast::Expr>, Option<&ast::Expr>) = match stmt {
            ast::Stmt::Assign(assign) => (assign.targets.iter().collect(), Some(&assign.value)),
            ast::Stmt::AnnAssign(assign) => (vec![&assign.target], assign.value.as_deref()),
            ast::Stmt::ClassDef(class) => return trips_s105(&class.body),
            _ => return false,
        };
        let string = value
            .and_then(ast::Expr::as_string_literal_expr)
            .is_some_and(|string| !string.value.to_str().is_empty());
        string
            && (targets.iter())
                .filter_map(|target| target.as_name_expr())
                .any(|name| SECRET.is_match(name.id.as_str()))
    })
}

/// Spells every docstring, and every other string holding a line break, as
/// one is read: ruff's unparser writes a line break in a string as `\n`, but
/// a docstring's lines, and a SQL statement's, belong on lines of their own.
///
/// A docstring's continuation lines are indented to its own column, and a
/// multi-line docstring closes on a line of its own (PEP 257). The formatter
/// would re-indent a class or function docstring, but leaves an attribute
/// docstring as written, so all are written finished. Any other string is
/// written verbatim: its value is data, such as the SQL a query runs.
///
/// Every string statement this crate generates is a docstring.
fn respell_strings(source: &str) -> String {
    #[derive(Default)]
    struct Strings(Vec<(TextRange, String, bool)>);
    impl<'a> Visitor<'a> for Strings {
        fn visit_stmt(&mut self, stmt: &'a ast::Stmt) {
            if let ast::Stmt::Expr(ast::StmtExpr { value, range, .. }) = stmt
                && let ast::Expr::StringLiteral(string) = &**value
            {
                return self.0.push((*range, string.value.to_str().into(), true));
            }
            walk_stmt(self, stmt);
        }
        fn visit_expr(&mut self, expr: &'a ast::Expr) {
            if let ast::Expr::StringLiteral(string) = expr
                && string.value.to_str().contains('\n')
            {
                return self
                    .0
                    .push((string.range, string.value.to_str().into(), false));
            }
            walk_expr(self, expr);
        }
    }

    let mut strings = Strings::default();
    let parsed = parse_module(source)
        .unwrap_or_else(|error| panic!("generated invalid Python ({error}):\n{source}"));
    strings.visit_body(&parsed.syntax().body);

    let mut respelt = source.to_string();
    for (range, text, docstring) in strings.0.into_iter().rev() {
        let start = range.start().to_usize();
        let column = start - source[..start].rfind('\n').map_or(0, |newline| newline + 1);
        respelt.replace_range(
            range.to_std_range(),
            &triple_quoted(&text, docstring.then_some(column)),
        );
    }
    respelt
}

/// The source of a triple-quoted string holding `text`: a docstring written at
/// the column given, or else `text` verbatim.
///
/// Only what a triple-quoted string cannot hold as written is escaped, plus
/// what linters reject even inside one, or a reader cannot see: a backslash; a
/// quote that would close the string early; whatever Python's
/// `str.isprintable()` rejects other than a space or a tab, as ruff's
/// unparser does in any other string (control and format characters, such as
/// the bidi controls of "Trojan Source", PLE2502; separators an editor breaks
/// a line at, U+2028, or shows as a space, U+00A0); whitespace ending a line
/// (W291); and a tab in the indentation of a line that also indents with
/// spaces (E101), which a docstring's continuation lines always do. A
/// docstring's lines lose their trailing whitespace instead.
fn triple_quoted(text: &str, docstring: Option<usize>) -> String {
    let lines: Vec<&str> = match docstring {
        Some(_) => text.lines().map(str::trim_end).collect(),
        None => text.split('\n').collect(),
    };
    let indent = " ".repeat(docstring.unwrap_or_default());

    let mut source = String::from(r#"""""#);
    for (n, line) in lines.iter().enumerate() {
        let last = n + 1 == lines.len();
        let indented = n > 0 && !indent.is_empty() && !line.is_empty();
        if n > 0 {
            source.push('\n');
        }
        if indented {
            source += &indent;
        }
        let indentation = &line[..line.len() - line.trim_start_matches([' ', '\t']).len()];
        let mixed = indentation.contains('\t') && (indented || indentation.contains(' '));
        let mut chars = line.char_indices().peekable();
        while let Some((i, c)) = chars.next() {
            let next = chars.peek().map(|&(_, next)| next);
            let ends_line = next.is_none() && !last;
            match c {
                '\\' => source += r"\\",
                '"' if next == Some('"') || (next.is_none() && last) => source += r#"\""#,
                '\t' if (mixed && i < indentation.len()) || ends_line => source += r"\t",
                ' ' if ends_line => source += r"\x20",
                ' ' | '\t' => source.push(c),
                c if !is_printable(c) => {
                    source += &match u32::from(c) {
                        code @ ..=0xff => format!(r"\x{code:02x}"),
                        code @ ..=0xffff => format!(r"\u{code:04x}"),
                        code => format!(r"\U{code:08x}"),
                    }
                }
                c => source.push(c),
            }
        }
    }
    if docstring.is_some() && lines.len() > 1 {
        source += &format!("\n{indent}");
    }
    source + r#"""""#
}

/// Whether Python's `str.isprintable()` holds of `c`: not a control, format,
/// private use, unassigned or separator character, the ASCII space excepted.
fn is_printable(c: char) -> bool {
    use icu_properties::props::{EnumeratedProperty, GeneralCategory as G};
    c == ' '
        || !matches!(
            G::for_char(c),
            G::Control
                | G::Format
                | G::Surrogate
                | G::PrivateUse
                | G::Unassigned
                | G::LineSeparator
                | G::ParagraphSeparator
                | G::SpaceSeparator
        )
}

// -- Fragments ---------------------------------------------------------------

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
/// already one ([`annotation`]). Schema data goes into a template's holes.
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
/// attribute it documents. [`render`] spells it.
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

// -- Types -------------------------------------------------------------------

/// A reference to a Python type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type {
    /// An annotation built from builtins and the [vocabulary](VOCABULARY), as
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

// -- Imports -----------------------------------------------------------------

/// Everything generated code may read without defining it, and how each is
/// imported (`docs/python-output.md` §2). [`render`] imports exactly the
/// entries its body reads, as written here: in isort's sections (the standard
/// library, third-party, relative), each listing plain `import`s before `from`
/// imports, by module, then by name.
///
/// `datetime` is bound as `dt`, ruff's convention (ICN001): a column called
/// `datetime` or `date` cannot hide it, and `dt.datetime` does not stutter.
const VOCABULARY: [&[Import]; 3] = [
    &[
        Import::Module("contextlib"),
        Import::Module("dataclasses"),
        Import::Module("datetime"),
        Import::Module("enum"),
        Import::Module("sqlite3"),
        Import::From("collections.abc", "Iterable"),
        Import::From("collections.abc", "Sequence"),
        Import::From("decimal", "Decimal"),
        Import::From("ipaddress", "IPv4Address"),
        Import::From("ipaddress", "IPv4Interface"),
        Import::From("ipaddress", "IPv4Network"),
        Import::From("ipaddress", "IPv6Address"),
        Import::From("ipaddress", "IPv6Interface"),
        Import::From("ipaddress", "IPv6Network"),
        Import::From("typing", "Any"),
        Import::From("typing", "Final"),
        Import::From("uuid", "UUID"),
    ],
    &[
        Import::Module("aiosqlite"),
        Import::Module("psycopg"),
        Import::Module("pymysql"),
        Import::Module("sqlalchemy"),
        Import::From("psycopg.rows", "tuple_row"),
        Import::From("psycopg.types.json", "Json"),
        Import::From("psycopg.types.json", "Jsonb"),
        Import::From("psycopg.types.range", "Range"),
        Import::From("sqlalchemy.dialects", "postgresql"),
        Import::From("sqlalchemy.ext.asyncio", "AsyncConnection"),
    ],
    // isort orders classes before modules.
    &[
        Import::From(".", "MultipleRowsError"),
        Import::From(".", "NoRowError"),
        Import::From(".", "models"),
    ],
];

/// Whether generated code imports something under `name`, which nothing it
/// defines may then take.
pub fn is_imported(name: &str) -> bool {
    (VOCABULARY.iter().copied().flatten()).any(|import| import.binding() == name)
}

/// One import statement's worth of a module, ordered as isort orders one
/// section.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Import {
    /// `import <module>`.
    Module(&'static str),
    /// `from <module> import <name>`; `<module>` may be relative, such as `.`.
    From(&'static str, &'static str),
}

impl Import {
    /// The name this import binds.
    fn binding(&self) -> &'static str {
        match *self {
            Self::Module("datetime") => "dt",
            Self::Module(module) => module,
            Self::From(_, name) => name,
        }
    }
}

/// The import block of a module whose body reads `body`'s names: the
/// [vocabulary](VOCABULARY) it reads, with names from one module merged, and
/// each of `modules` whose first name it reads, in the standard library's
/// section or the third-party one; a blank line between sections.
///
/// It is the one part of a module besides the header written as text: every
/// name in it is a constant of this crate or a checked dotted name, and the
/// blank line between sections is layout a tree cannot carry.
fn imports(body: &[ast::Stmt], modules: &[String]) -> String {
    #[derive(Default)]
    struct Reads(BTreeSet<String>);
    impl<'a> Visitor<'a> for Reads {
        fn visit_expr(&mut self, expr: &'a ast::Expr) {
            if let ast::Expr::Name(name) = expr
                && name.ctx.is_load()
            {
                self.0.insert(name.id.to_string());
            }
            walk_expr(self, expr);
        }
    }
    let mut reads = Reads::default();
    reads.visit_body(body);

    let sections: Vec<String> = (VOCABULARY.iter().enumerate())
        .map(|(at, section)| {
            // isort's order: each `import` by its module, then each `from`.
            let mut plain: Vec<(String, String)> = Vec::new();
            let mut froms: Vec<String> = Vec::new();
            let mut previous: Option<Import> = None;
            for &import in section.iter().filter(|i| reads.0.contains(i.binding())) {
                match (import, previous) {
                    (Import::From(module, name), Some(Import::From(last, _))) if last == module => {
                        *froms.last_mut().expect("a previous import") += &format!(", {name}");
                    }
                    (Import::From(module, name), _) => {
                        froms.push(format!("from {module} import {name}"));
                    }
                    (Import::Module(module), _) => match import.binding() {
                        alias if alias == module => {
                            plain.push((module.into(), format!("import {module}")))
                        }
                        alias => plain.push((module.into(), format!("import {module} as {alias}"))),
                    },
                }
                previous = Some(import);
            }
            let stdlib = |module: &str| {
                let root = module.split('.').next().unwrap_or(module);
                ruff_python_stdlib::sys::is_known_standard_library(10, root)
            };
            for module in modules {
                let root = module.split('.').next().unwrap_or(module);
                let section = if stdlib(module) { 0 } else { 1 };
                if section == at
                    && reads.0.contains(root)
                    && !plain.iter().any(|(m, _)| m == module)
                {
                    plain.push((module.clone(), format!("import {module}")));
                }
            }
            plain.sort_by_key(|(module, _)| module.to_lowercase());
            (plain.into_iter().map(|(_, line)| line).chain(froms))
                .map(|line| format!("{line}\n"))
                .collect()
        })
        .filter(|section: &String| !section.is_empty())
        .collect();
    sections.join("\n")
}
