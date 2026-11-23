//! Every name generated code reads or binds that is not the schema's, each spelt exactly once: the builtins it reads,
//! what it imports ([`VOCABULARY`]), and the parameters, locals and methods of its own.

use ruff_python_ast as ast;

use super::{Expression, name};

#[cfg(test)]
#[path = "vocabulary_spec.rs"]
mod spec;

/// A name with dots is read through its first part, as in `dt.date`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Symbol(&'static str);

impl AsRef<str> for Symbol {
    fn as_ref(&self) -> &str {
        self.0
    }
}

impl From<Symbol> for ast::Expr {
    fn from(symbol: Symbol) -> Self {
        let mut parts = symbol.0.split('.');
        let root = name(parts.next().unwrap_or_default());
        parts.fold(root, Expression::attr)
    }
}

pub const BASE_EXCEPTION: Symbol = Symbol("BaseException");
pub const BOOL: Symbol = Symbol("bool");
pub const BYTES: Symbol = Symbol("bytes");
pub const CLASSMETHOD: Symbol = Symbol("classmethod");
pub const DICT: Symbol = Symbol("dict");
pub const FLOAT: Symbol = Symbol("float");
pub const INT: Symbol = Symbol("int");
pub const ISINSTANCE: Symbol = Symbol("isinstance");
pub const LEN: Symbol = Symbol("len");
pub const LIST: Symbol = Symbol("list");
pub const LOOKUP_ERROR: Symbol = Symbol("LookupError");
pub const OBJECT: Symbol = Symbol("object");
pub const STR: Symbol = Symbol("str");
pub const TUPLE: Symbol = Symbol("tuple");
pub const TYPE_ERROR: Symbol = Symbol("TypeError");

pub const CONTEXTLIB: Symbol = Symbol("contextlib");
pub const DATACLASSES: Symbol = Symbol("dataclasses");
/// We bind `datetime` as `dt`, ruff's convention (ICN001), so a column called `datetime` or `date` cannot hide it and
/// `dt.datetime` does not stutter.
pub const DT: Symbol = Symbol("dt");
pub const ENUM: Symbol = Symbol("enum");
pub const SQLITE3: Symbol = Symbol("sqlite3");
pub const TYPING: Symbol = Symbol("typing");
pub const ASYNC_GENERATOR: Symbol = Symbol("AsyncGenerator");
pub const ASYNC_ITERATOR: Symbol = Symbol("AsyncIterator");
pub const GENERATOR: Symbol = Symbol("Generator");
pub const ITERABLE: Symbol = Symbol("Iterable");
pub const ITERATOR: Symbol = Symbol("Iterator");
pub const MAPPING: Symbol = Symbol("Mapping");
pub const SEQUENCE: Symbol = Symbol("Sequence");
pub const DECIMAL: Symbol = Symbol("Decimal");
pub const IPV4_ADDRESS: Symbol = Symbol("IPv4Address");
pub const IPV4_INTERFACE: Symbol = Symbol("IPv4Interface");
pub const IPV4_NETWORK: Symbol = Symbol("IPv4Network");
pub const IPV6_ADDRESS: Symbol = Symbol("IPv6Address");
pub const IPV6_INTERFACE: Symbol = Symbol("IPv6Interface");
pub const IPV6_NETWORK: Symbol = Symbol("IPv6Network");
pub const ANY: Symbol = Symbol("Any");
pub const FINAL: Symbol = Symbol("Final");
pub const UUID: Symbol = Symbol("UUID");

pub const AIOSQLITE: Symbol = Symbol("aiosqlite");
pub const ASYNCMY: Symbol = Symbol("asyncmy");
pub const PSYCOPG: Symbol = Symbol("psycopg");
pub const PYDANTIC: Symbol = Symbol("pydantic");
/// What pydantic reads a model's configuration from.
pub const MODEL_CONFIG: Symbol = Symbol("model_config");
pub const PYMYSQL: Symbol = Symbol("pymysql");
pub const SQLALCHEMY: Symbol = Symbol("sqlalchemy");
pub const TUPLE_ROW: Symbol = Symbol("tuple_row");
pub const JSON: Symbol = Symbol("Json");
pub const JSONB: Symbol = Symbol("Jsonb");
pub const RANGE: Symbol = Symbol("Range");
pub const POSTGRESQL: Symbol = Symbol("postgresql");
pub const ASYNC_CONNECTION: Symbol = Symbol("AsyncConnection");

pub const MULTIPLE_ROWS_ERROR: Symbol = Symbol("MultipleRowsError");
pub const NO_ROW_ERROR: Symbol = Symbol("NoRowError");
pub const MODELS: Symbol = Symbol("models");

/// Read through [`TYPING`] because, imported by name, a table `protocols` could no longer be the class `Protocol`.
pub const PROTOCOL: Symbol = Symbol("typing.Protocol");
pub const TYPE_CHECKING: Symbol = Symbol("typing.TYPE_CHECKING");

pub const DATE: Symbol = Symbol("dt.date");
pub const DATETIME: Symbol = Symbol("dt.datetime");
pub const TIME: Symbol = Symbol("dt.time");
pub const TIMEDELTA: Symbol = Symbol("dt.timedelta");

/// Everything generated code may read without defining it, and how each is imported.
/// [`super::render()`] imports exactly the entries a body reads, in the order written here, so this list must stay in
/// isort's order: its sections (standard library, third-party, relative), plain `import`s before `from` imports, by
/// module, then by name.
pub(super) const VOCABULARY: [&[Import]; 3] = [
    &[
        Import::plain(CONTEXTLIB),
        Import::plain(DATACLASSES),
        Import::Module("datetime", DT),
        Import::plain(ENUM),
        Import::plain(SQLITE3),
        Import::plain(TYPING),
        Import::From("collections.abc", ASYNC_GENERATOR),
        Import::From("collections.abc", ASYNC_ITERATOR),
        Import::From("collections.abc", GENERATOR),
        Import::From("collections.abc", ITERABLE),
        Import::From("collections.abc", ITERATOR),
        Import::From("collections.abc", MAPPING),
        Import::From("collections.abc", SEQUENCE),
        Import::From("decimal", DECIMAL),
        Import::From("ipaddress", IPV4_ADDRESS),
        Import::From("ipaddress", IPV4_INTERFACE),
        Import::From("ipaddress", IPV4_NETWORK),
        Import::From("ipaddress", IPV6_ADDRESS),
        Import::From("ipaddress", IPV6_INTERFACE),
        Import::From("ipaddress", IPV6_NETWORK),
        Import::From("typing", ANY),
        Import::From("typing", FINAL),
        Import::From("uuid", UUID),
    ],
    &[
        Import::plain(AIOSQLITE),
        // A type checker only sees the cursors as an attribute of `asyncmy` once their module is imported, and that
        // import binds `asyncmy` too.
        Import::Module("asyncmy.cursors", ASYNCMY),
        Import::plain(PSYCOPG),
        Import::plain(PYDANTIC),
        Import::plain(PYMYSQL),
        Import::plain(SQLALCHEMY),
        Import::From("psycopg.rows", TUPLE_ROW),
        Import::From("psycopg.types.json", JSON),
        Import::From("psycopg.types.json", JSONB),
        Import::From("psycopg.types.range", RANGE),
        Import::From("sqlalchemy.dialects", POSTGRESQL),
        Import::From("sqlalchemy.ext.asyncio", ASYNC_CONNECTION),
    ],
    &[Import::From(".", MODELS)],
];

/// The derived order is isort's order within one section.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Import {
    /// `import <module>`, with an `as <symbol>` when the symbol is not the module's first part.
    Module(&'static str, Symbol),
    /// `from <module> import <symbol>`, where the module may be relative (`.`).
    From(&'static str, Symbol),
}

impl Import {
    const fn plain(module: Symbol) -> Self {
        Self::Module(module.0, module)
    }

    pub(super) fn binding(&self) -> &'static str {
        let (Self::Module(_, symbol) | Self::From(_, symbol)) = self;
        symbol.0
    }
}

/// Nothing generated may define a name that is imported.
pub fn is_imported(name: &str) -> bool {
    (VOCABULARY.iter().copied().flatten()).any(|import| import.binding() == name)
}

// A function's parameters (`connection`, then `params` or `rows`) are part of the generated API; the rest are locals a
// body binds for itself.

pub const CONNECTION: Symbol = Symbol("connection");
/// The one record of a function's parameters and, on SQLAlchemy, a batch's rows as it binds them.
pub const PARAMS: Symbol = Symbol("params");
pub const ROWS: Symbol = Symbol("rows");
pub const R: Symbol = Symbol("r");

// NOTE: We do NOT call this `cursor`, as a query's keyword parameter is often named `cursor` (pagination) and a
// reserved `cursor` would rename the user's parameter to `cursor_`.
pub const CUR: Symbol = Symbol("cur");
/// A row as the database returns it.
pub const ROW: Symbol = Symbol("row");
pub const VALUE: Symbol = Symbol("value");
pub const VALUES: Symbol = Symbol("values");
pub const COUNT: Symbol = Symbol("count");
pub const RESULT: Symbol = Symbol("result");
pub const COPY: Symbol = Symbol("copy");

pub const CLS: Symbol = Symbol("cls");
pub const SELF: Symbol = Symbol("self");
pub const FROM_ROW: Symbol = Symbol("from_row");
/// An enum's constructor of its members from an array's [`TEXT`], which is what psycopg returns for an enum array.
pub const FROM_ARRAY: Symbol = Symbol("from_array");
pub const TEXT: Symbol = Symbol("text");
pub const LABEL: Symbol = Symbol("label");
pub const V: Symbol = Symbol("v");

/// What reads an array a driver returned as lists and fails for one of another depth than its column declares, with
/// its parameters and locals.
pub const CHECKED_ARRAY: Symbol = Symbol("_checked_array");
pub const DIMENSIONS: Symbol = Symbol("dimensions");
/// Whether an element may itself be a list, which is then no deeper array.
pub const LIST_LEAVES: Symbol = Symbol("list_leaves");
pub const ITEMS: Symbol = Symbol("items");
pub const CHILD: Symbol = Symbol("child");
pub const MESSAGE: Symbol = Symbol("message");

/// The connection's class, for where it is only generic in the library's stubs.
pub const CONNECTION_TYPE: Symbol = Symbol("_Connection");
/// A protocol of a cursor's methods as generated code calls them, a function that views a cursor as it, and its
/// parameter, for drivers whose stubs leave those methods untyped (see "Workarounds" in AGENTS.md).
pub const TYPED_CURSOR: Symbol = Symbol("_Cursor");
pub const TYPED: Symbol = Symbol("_typed");
pub const CURSOR: Symbol = Symbol("cursor");
