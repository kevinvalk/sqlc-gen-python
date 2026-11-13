//! Mapping PostgreSQL column types to Python types.
//!
//! One lookup: the type name sqlc reports, to a [`Type`], plus whether the
//! drivers load an array of it as a list. Nullability and array depth are not
//! its concern — in Python those are `T | None` and `list[T]` whatever the
//! type — so they are applied once, in [`crate::api::models`].
//!
//! PostgreSQL's catalog reports some types both bare and
//! `pg_catalog`-qualified, and by more than one name, depending on how the
//! column was declared and how sqlc analysed it.

use crate::python::Type;

#[cfg(test)]
#[path = "types_spec.rs"]
mod spec;

/// The type map of `docs/python-output.md` §5, keyed by PostgreSQL's own name
/// for each type (`pg_type.typname`): what psycopg (sync and async) and asyncpg
/// were observed to return, as a scalar and, outside [`NO_ARRAYS`], as an
/// array. A type nobody has probed is not here. `tests/schemas/src/pgtypes`
/// has a column of each, and an array column of each that arrays allow, so the
/// runtime proof holds every entry to what the drivers return.
const TYPES: &[(&str, Type)] = &[
    // Python has one integer type, so width only matters to the database.
    ("int2", Type::INT),
    ("int4", Type::INT),
    ("int8", Type::INT),
    ("oid", Type::INT),
    ("float4", Type::FLOAT),
    ("float8", Type::FLOAT),
    // Exact, so a float would silently lose precision.
    ("numeric", Type::DECIMAL),
    // The drivers return the locale-formatted text (`'$12.34'`), never a number.
    ("money", Type::STR),
    ("bool", Type::BOOL),
    ("text", Type::STR),
    ("varchar", Type::STR),
    ("bpchar", Type::STR),
    ("name", Type::STR),
    ("citext", Type::STR),
    ("macaddr", Type::STR),
    ("macaddr8", Type::STR),
    ("xml", Type::STR),
    ("tsvector", Type::STR),
    ("bytea", Type::BYTES),
    ("date", Type::DATE),
    ("time", Type::TIME),
    ("timetz", Type::TIME),
    ("timestamp", Type::DATETIME),
    ("timestamptz", Type::DATETIME),
    ("interval", Type::TIMEDELTA),
    ("uuid", Type::UUID),
    // The driver decodes JSON with a configurable decoder.
    ("json", Type::JSON),
    ("jsonb", Type::JSONB),
    ("inet", Type::INET),
    ("cidr", Type::CIDR),
];

/// Types whose arrays the drivers do not all load as a list of the type, and
/// what one of them does instead.
const NO_ARRAYS: &[(&str, &str)] = &[
    // An extension's type has a different OID in every database, so psycopg
    // has no loader for an array of one.
    ("citext", "psycopg returns as text, not a list"),
    (
        "tsvector",
        "asyncpg decodes with a corrupt item when one is empty",
    ),
];

/// The Python type of a column of `data_type`, or `None` if nobody has probed
/// what the drivers return for it.
pub fn scalar_type(data_type: &str) -> Option<Type> {
    let name = typname(data_type);
    TYPES
        .iter()
        .find(|(known, _)| *known == name)
        .map(|(_, python)| python.clone())
}

/// What a driver does with an array of `data_type`, if that is not a list.
pub fn array_problem(data_type: &str) -> Option<&'static str> {
    let name = typname(data_type);
    (NO_ARRAYS.iter())
        .find(|(known, _)| *known == name)
        .map(|(_, problem)| *problem)
}

/// Whether `data_type` is sqlc's guess rather than a type it read
/// (`docs/python-output.md` §5, rule 2).
///
/// sqlc's static analyser writes a declared or cast type as PostgreSQL's own
/// name, `pg_catalog`-qualified where the grammar spells it as a keyword
/// (`bigint` is `pg_catalog.int8`, `timestamptz` stays bare). It writes SQL's
/// standard name only for a type it read off a function's signature, the
/// first overload it lists with that many arguments, whatever the call
/// resolves to; the guess survives a view, a CTE and a subquery, where
/// `is_func_call` does not. A bare `int` (its report of any arithmetic
/// expression), `any` and nothing are guesses too. `integer` is not: sqlc
/// types LIMIT and OFFSET with it itself.
pub fn is_guess(data_type: &str) -> bool {
    [
        "",
        "any",
        "anyelement",
        "anyarray",
        "int",
        "smallint",
        "bigint",
        "real",
        "double precision",
        "numeric",
        "boolean",
        "character",
        "character varying",
        "interval",
        "time with time zone",
        "time without time zone",
        "timestamp with time zone",
        "timestamp without time zone",
    ]
    .contains(&data_type)
}

/// PostgreSQL's own name for a type, from any spelling sqlc reports for one it
/// read ([`is_guess`] is false): qualified or bare, a serial pseudo-type, or
/// LIMIT's `integer`.
fn typname(data_type: &str) -> &str {
    match data_type.strip_prefix("pg_catalog.").unwrap_or(data_type) {
        "smallserial" | "serial2" => "int2",
        "integer" | "serial" | "serial4" => "int4",
        "bigserial" | "serial8" => "int8",
        name => name,
    }
}
