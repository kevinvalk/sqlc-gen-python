//! Nullability and array depth are not ours to map: in Python they are `T | None` and `list[T]` whatever the type, so
//! [`crate::api::models`] applies them once.

use crate::python::Type;

#[cfg(test)]
#[path = "types_spec.rs"]
mod spec;

/// What psycopg (sync and async) and asyncpg were seen to return, as a scalar and (outside [`NO_ARRAYS`]) as an array,
/// keyed by PostgreSQL's own name (`pg_type.typname`). A type nobody has probed does not belong here.
/// `examples/pgtypes` has a column and an array column of each, so the runtime proof holds every entry to what the
/// drivers return.
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
    // Both drivers return an extension's `ltree` as its text, and a `regclass` as its name. We leave `hstore` to the
    // backend, as SQLAlchemy registers psycopg's codec for it.
    ("ltree", Type::STR),
    ("regclass", Type::STR),
    ("bytea", Type::BYTES),
    ("date", Type::DATE),
    ("time", Type::TIME),
    ("timetz", Type::TIME),
    ("timestamp", Type::DATETIME),
    ("timestamptz", Type::DATETIME),
    ("interval", Type::TIMEDELTA),
    ("uuid", Type::UUID),
    // The drivers decode JSON with a configurable decoder, so `object` is the one annotation true under every one. A
    // recursive `JsonValue` alias fails `get_type_hints` across modules, and pydantic on Python 3.10.
    ("json", Type::JSON),
    ("jsonb", Type::JSONB),
    ("inet", Type::INET),
    ("cidr", Type::CIDR),
];

/// Types whose arrays the drivers do not all load as a list of the type, and
/// what one of them does instead.
const NO_ARRAYS: &[(&str, &str)] = &[
    // An extension's type has a different OID in every database, so psycopg has no loader for an array of one
    ("citext", "psycopg returns as text, not a list"),
    ("hstore", "psycopg returns as text, not a list"),
    ("ltree", "psycopg returns as text, not a list"),
    ("regclass", "nobody has probed"),
    ("tsvector", "asyncpg decodes with a corrupt item when one is empty"),
];

/// `None` when nobody has probed what the drivers return for `data_type`.
pub fn scalar_type(data_type: &str) -> Option<Type> {
    let name = type_name(data_type);
    TYPES
        .iter()
        .find(|(known, _)| *known == name)
        .map(|(_, python)| python.clone())
}

/// What a driver does with an array of `data_type`, if that is not a list.
pub fn array_problem(data_type: &str) -> Option<&'static str> {
    let name = type_name(data_type);
    (NO_ARRAYS.iter())
        .find(|(known, _)| *known == name)
        .map(|(_, problem)| *problem)
}

/// Whether `data_type` is sqlc's guess instead of a type it read.
///
/// sqlc's static analyser writes a declared or cast type by PostgreSQL's own name, `pg_catalog`-qualified where the
/// grammar spells it as a keyword (`bigint` is `pg_catalog.int8`, `timestamptz` stays bare). It only writes SQL's
/// standard name for a type it took off a function's signature: the first overload with that many arguments, whatever
/// the call resolves to. That guess survives a view, a CTE and a subquery, where `is_func_call` does not. A bare `int`
/// (its type for any arithmetic), `any` and nothing are guesses too. `integer` alone is not, as sqlc types some
/// parameters with it itself (`Engine::is_guess`).
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

/// PostgreSQL's own name for a type sqlc read ([`is_guess`] is false). sqlc reports one bare or `pg_catalog`-qualified,
/// and by more than one name (a serial pseudo-type, a `LIMIT`'s `integer`), depending on how the column was declared
/// and how sqlc analysed it.
pub fn type_name(data_type: &str) -> String {
    let name = data_type.strip_prefix("pg_catalog.").unwrap_or(data_type);
    match name {
        "smallserial" | "serial2" => "int2",
        "integer" | "serial" | "serial4" => "int4",
        "bigserial" | "serial8" => "int8",
        name => name,
    }
    .to_string()
}
