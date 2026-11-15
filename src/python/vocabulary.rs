//! Every name generated code reads or binds that is not the schema's, each
//! spelt once: the builtins it reads, what it imports ([`VOCABULARY`]), and
//! the parameters, locals and methods of its own.
//!
//! A [`Symbol`] reads as the name it is, so `ROW.index(int(0))` is `row[0]`
//! ([`super::Expression`]), and binds as it too: `each(ROW, rows)` is `for row
//! in rows`.

use ruff_python_ast as ast;

use super::{Expression, name};

#[cfg(test)]
#[path = "vocabulary_spec.rs"]
mod spec;

/// A name, by its one spelling. One with dots is read through its first
/// part: `dt.date`.
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

// -- Builtins ----------------------------------------------------------------

pub const BOOL: Symbol = Symbol("bool");
pub const BYTES: Symbol = Symbol("bytes");
pub const CLASSMETHOD: Symbol = Symbol("classmethod");
pub const DICT: Symbol = Symbol("dict");
pub const FLOAT: Symbol = Symbol("float");
pub const INT: Symbol = Symbol("int");
pub const LEN: Symbol = Symbol("len");
pub const LIST: Symbol = Symbol("list");
pub const LOOKUP_ERROR: Symbol = Symbol("LookupError");
pub const OBJECT: Symbol = Symbol("object");
pub const STR: Symbol = Symbol("str");
pub const TUPLE: Symbol = Symbol("tuple");

// -- Imports -----------------------------------------------------------------

pub const CONTEXTLIB: Symbol = Symbol("contextlib");
pub const DATACLASSES: Symbol = Symbol("dataclasses");
/// `datetime`, bound as ruff's convention has it (ICN001): a column called
/// `datetime` or `date` cannot hide it, and `dt.datetime` does not stutter.
pub const DT: Symbol = Symbol("dt");
pub const ENUM: Symbol = Symbol("enum");
pub const SQLITE3: Symbol = Symbol("sqlite3");
pub const TYPING: Symbol = Symbol("typing");
pub const ITERABLE: Symbol = Symbol("Iterable");
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

/// Read through [`TYPING`]: imported by name, a table `protocols` could not
/// be the class `Protocol`.
pub const PROTOCOL: Symbol = Symbol("typing.Protocol");
pub const TYPE_CHECKING: Symbol = Symbol("typing.TYPE_CHECKING");

/// `datetime`'s classes, read through [`DT`].
pub const DATE: Symbol = Symbol("dt.date");
pub const DATETIME: Symbol = Symbol("dt.datetime");
pub const TIME: Symbol = Symbol("dt.time");
pub const TIMEDELTA: Symbol = Symbol("dt.timedelta");

/// Everything generated code may read without defining it, and how each is
/// imported (`docs/python-output.md` §2). [`super::render()`] imports exactly the
/// entries its body reads, as written here: in isort's sections (the standard
/// library, third-party, relative), each listing plain `import`s before `from`
/// imports, by module, then by name.
pub(super) const VOCABULARY: [&[Import]; 3] = [
    &[
        Import::plain(CONTEXTLIB),
        Import::plain(DATACLASSES),
        Import::Module("datetime", DT),
        Import::plain(ENUM),
        Import::plain(SQLITE3),
        Import::plain(TYPING),
        Import::From("collections.abc", ITERABLE),
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
        // Its cursors are no attribute of `asyncmy` to a type checker
        // without the import of their module, which binds `asyncmy` too.
        Import::Module("asyncmy.cursors", ASYNCMY),
        Import::plain(PSYCOPG),
        Import::plain(PYMYSQL),
        Import::plain(SQLALCHEMY),
        Import::From("psycopg.rows", TUPLE_ROW),
        Import::From("psycopg.types.json", JSON),
        Import::From("psycopg.types.json", JSONB),
        Import::From("psycopg.types.range", RANGE),
        Import::From("sqlalchemy.dialects", POSTGRESQL),
        Import::From("sqlalchemy.ext.asyncio", ASYNC_CONNECTION),
    ],
    // isort orders classes before modules.
    &[
        Import::From(".", MULTIPLE_ROWS_ERROR),
        Import::From(".", NO_ROW_ERROR),
        Import::From(".", MODELS),
    ],
];

/// One name's import, ordered as isort orders one section.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Import {
    /// `import <module>`, bound as the symbol: `as` it, if that is not the
    /// name the import binds itself, the module's first part.
    Module(&'static str, Symbol),
    /// `from <module> import <symbol>`; `<module>` may be relative, such as
    /// `.`.
    From(&'static str, Symbol),
}

impl Import {
    /// `import <module>`, bound as itself.
    const fn plain(module: Symbol) -> Self {
        Self::Module(module.0, module)
    }

    /// The name this import binds.
    pub(super) fn binding(&self) -> &'static str {
        let (Self::Module(_, symbol) | Self::From(_, symbol)) = self;
        symbol.0
    }
}

/// Whether generated code imports something under `name`, which nothing it
/// defines may then take.
pub fn is_imported(name: &str) -> bool {
    (VOCABULARY.iter().copied().flatten()).any(|import| import.binding() == name)
}

// -- Its own -----------------------------------------------------------------
//
// A function's parameters (`conn`, then `params` or `rows`) are the generated
// API's; the rest are names a body binds for itself.

pub const CONN: Symbol = Symbol("conn");
/// The one record of a function's parameters; and, on SQLAlchemy, a batch's
/// rows as it binds them.
pub const PARAMS: Symbol = Symbol("params");
pub const ROWS: Symbol = Symbol("rows");
/// One of [`ROWS`].
pub const R: Symbol = Symbol("r");

pub const CUR: Symbol = Symbol("cur");
/// A row as the database returns it.
pub const ROW: Symbol = Symbol("row");
pub const VALUE: Symbol = Symbol("value");
pub const VALUES: Symbol = Symbol("values");
/// One row's result of a batch.
pub const RESULT: Symbol = Symbol("result");
pub const COPY: Symbol = Symbol("copy");

pub const CLS: Symbol = Symbol("cls");
pub const SELF: Symbol = Symbol("self");
/// A record's constructor from a [`ROW`].
pub const FROM_ROW: Symbol = Symbol("from_row");
/// An enum's constructor of its members from an array's [`TEXT`].
pub const FROM_ARRAY: Symbol = Symbol("from_array");
pub const TEXT: Symbol = Symbol("text");
pub const LABEL: Symbol = Symbol("label");
/// One element of an array.
pub const V: Symbol = Symbol("v");

/// The connection's class, where it is generic in the library's stubs alone.
pub const CONNECTION: Symbol = Symbol("_Connection");
/// A cursor's methods as generated code calls them, where the driver's own
/// stubs leave them untyped; what views a cursor so; and the cursor it views.
pub const TYPED_CURSOR: Symbol = Symbol("_Cursor");
pub const TYPED: Symbol = Symbol("_typed");
pub const CURSOR: Symbol = Symbol("cursor");
