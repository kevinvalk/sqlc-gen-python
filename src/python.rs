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
//! places. For the fixed parts there is nothing to build: [`expr`] and [`stmt`]
//! hand the text to ruff's parser, so the source reads as the Python it
//! produces. Only where schema data lands is a node constructed by hand, and
//! only because a column name is not guaranteed to be a Python identifier.
//!
//! [`render`] closes the loop: ruff unparses the tree, then ruff formats it.

use std::collections::BTreeSet;

use ruff_python_ast::visitor::{Visitor, walk_expr, walk_stmt};
use ruff_python_ast::{self as ast, name::Name};
use ruff_python_codegen::{Generator, Indentation};
use ruff_python_formatter::{PyFormatOptions, format_module_source};
use ruff_python_parser::{parse_expression, parse_module};
use ruff_source_file::LineEnding;
use ruff_text_size::{TextRange, TextSize};
use thin_vec::ThinVec;

#[cfg(test)]
#[path = "python_spec.rs"]
mod spec;

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

/// A name a module or package can be imported by: a public name in lowercase
/// (ruff N999), so that no two are one file on a case-insensitive file system.
pub fn is_module_name(name: &str) -> bool {
    is_public_name(name) && !name.chars().any(|c| c.is_ascii_uppercase())
}

/// A bare name, such as `int` or `row`.
fn name(id: &str) -> ast::Expr {
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

/// A call with positional arguments, such as `sqlalchemy.text("SELECT 1")`.
pub fn call(func: ast::Expr, args: impl IntoIterator<Item = ast::Expr>) -> ast::Expr {
    ast::ExprCall {
        func: Box::new(func),
        arguments: node!(ast::Arguments {
            args: args.into_iter().collect(),
            keywords: Default::default(),
        }),
        range_start: TextSize::default(),
        node_index: ast::AtomicNodeIndex::default(),
    }
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

    /// The source naming this type from within `scope`.
    pub fn source(&self, scope: Scope) -> String {
        match (self, scope) {
            (Self::Python(source) | Self::Converted(source), _) => (*source).into(),
            (Self::Json(_), _) => "object".into(),
            (Self::Custom { annotation, .. } | Self::Library { annotation, .. }, _) => {
                annotation.clone()
            }
            (Self::Enum(class), Scope::Models) => class.clone(),
            (Self::Enum(class), Scope::Queries) => format!("models.{class}"),
        }
    }
}

/// The modules an override's `annotation` reads names from, to import: of
/// each dotted name, all but its last part (`numpy.typing.NDArray` reads
/// `numpy.typing`). Any other name must be a builtin or one generated code
/// imports, and the annotation no more than names, subscripts, `|` and `None`.
pub fn modules_of(annotation: &str) -> anyhow::Result<Vec<String>> {
    fn walk(expr: &ast::Expr, modules: &mut Vec<String>) -> anyhow::Result<()> {
        match expr {
            ast::Expr::Name(name) => {
                let id = name.id.as_str();
                if !ruff_python_stdlib::builtins::is_python_builtin(id, 10, false)
                    && !is_imported(id)
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
                if !is_imported(&root.id) {
                    parts.push(root.id.as_str());
                    parts.reverse();
                    parts.pop();
                    let module = parts.join(".");
                    if !modules.contains(&module) {
                        modules.push(module);
                    }
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
    let expr = parse_expression(annotation)
        .map_err(|error| anyhow::anyhow!("not a Python expression: {error}"))?
        .into_syntax()
        .body;
    let mut modules = Vec::new();
    walk(&expr, &mut modules)?;
    Ok(modules)
}

/// The source of an expression, for an error.
fn python_source(expr: &ast::Expr) -> String {
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
            if let ast::Expr::Name(name) = expr {
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
