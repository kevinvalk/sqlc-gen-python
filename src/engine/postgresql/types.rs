//! Nullability and array depth are not ours to map: in Python they are `T | None` and `list[T]` whatever the type, so
//! [`crate::api::models`] applies them once.

use crate::python::Type;

#[cfg(test)]
#[path = "types_spec.rs"]
mod spec;

/// What psycopg (sync and async) and asyncpg were seen to return, as a scalar and (but for [`array_problem`]) as an
/// array, keyed by PostgreSQL's own name (`pg_type.typname`). A type nobody has probed does not belong here.
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
    // Both drivers return the `ltree` extension's types as their text, and a `regclass` as its name. We leave `hstore`
    // to the backend, as SQLAlchemy registers psycopg's codec for it.
    ("ltree", Type::STR),
    ("lquery", Type::STR),
    ("ltxtquery", Type::STR),
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
    // recursive `JsonValue` alias fails `get_type_hints` across modules.
    ("json", Type::JSON),
    ("jsonb", Type::JSONB),
    ("inet", Type::INET),
    ("cidr", Type::CIDR),
];

/// Types whose arrays the drivers do not all load as a list of the type, and what one of them does instead.
const NO_ARRAYS: &[(&str, &str)] = &[
    // Registered, psycopg loads an array of it as a list of the text of each, not of a `dict`
    (
        "hstore",
        "psycopg, and SQLAlchemy on it, returns as text, not a list of dict",
    ),
    ("ltxtquery", "nobody has probed"),
    ("regclass", "nobody has probed"),
    ("tsvector", "asyncpg decodes with a corrupt item when one is empty"),
];

/// Whether `name`, bare, is a type of PostgreSQL's own that we map, which no other schema's type of that name is.
pub fn is_own(name: &str) -> bool {
    !EXTENSIONS.contains(&name) && TYPES.iter().any(|(known, _)| *known == name)
}

/// `None` when nobody has probed what the drivers return for `data_type`.
pub fn scalar_type(data_type: &str) -> Option<Type> {
    let name = type_name(data_type);
    TYPES
        .iter()
        .find(|(known, _)| *known == name)
        .map(|(_, python)| python.clone())
}

/// Of [`NO_ARRAYS`], those every driver binds a list of `str` to, as a parameter, which no driver then reads back.
/// `examples/pgtypes` has a query of each, which the runtime sweep calls.
const BOUND_AS_LISTS: &[&str] = &["tsvector"];

/// Whether generated code registers `data_type` on a psycopg connection before it reads an array of it: any type but
/// PostgreSQL's own, so an extension's or the schema's. psycopg knows the OID of PostgreSQL's own types only, as every
/// other type has a different one in every database, so until then it returns an array of one as the array's text
/// (https://www.psycopg.org/psycopg3/docs/basic/pgtypes.html). asyncpg looks such a type up itself.
pub fn is_registered(data_type: &str) -> bool {
    !is_own(&type_name(data_type))
}

/// Types whose array asyncpg reads with a corrupt item where one is empty (`ARRAY[''::citext]`): it counts the item,
/// but reading it never returns. Registering the type with asyncpg (`set_type_codec`) changes nothing, and psycopg reads
/// them.
const ASYNCPG_CORRUPTS: &[&str] = &["citext", "lquery", "ltree"];

/// What a driver does with an array of `data_type`, if that is not a list, on a backend that may run on asyncpg when
/// `asyncpg`.
pub fn array_problem(data_type: &str, is_parameter: bool, asyncpg: bool) -> Option<&'static str> {
    let name = type_name(data_type);
    if is_parameter && BOUND_AS_LISTS.contains(&name.as_str()) {
        return None;
    }
    if asyncpg && !is_parameter && ASYNCPG_CORRUPTS.contains(&name.as_str()) {
        return Some("asyncpg, which an async SQLAlchemy may run on, reads with a corrupt item where one is empty");
    }
    (NO_ARRAYS.iter())
        .find(|(known, _)| *known == name)
        .map(|(_, problem)| *problem)
}

/// The types of [`TYPES`] and the backends' maps that an extension creates, in whichever schema it was created in.
const EXTENSIONS: &[&str] = &["citext", "hstore", "lquery", "ltree", "ltxtquery"];

/// PostgreSQL's own name for a type sqlc read. sqlc reports one bare or `pg_catalog`-qualified,
/// and by more than one name (a serial pseudo-type, a `LIMIT`'s `integer`), depending on how the column was declared
/// and how sqlc analysed it. An override names a type by any of those, or by SQL's standard name as the schema spells
/// it (`bigint`), which from sqlc is a guess and so never read here.
///
/// IMPORTANT: A type qualified by any other schema is never PostgreSQL's own, but a domain or an extension's, and sqlc
/// reports a domain by its name alone (`archive.money` may be a `numeric`). So we keep that schema, and drop it only
/// for an extension's type (`public.citext`, `extensions.citext`), which no domain shares a name with.
pub fn type_name(data_type: &str) -> String {
    let name = match data_type.rsplit_once('.') {
        Some(("pg_catalog", name)) => name,
        Some((_, name)) if EXTENSIONS.contains(&name) => name,
        _ => data_type,
    };
    match name {
        "smallint" | "smallserial" | "serial2" => "int2",
        "int" | "integer" | "serial" | "serial4" => "int4",
        "bigint" | "bigserial" | "serial8" => "int8",
        "real" => "float4",
        "double precision" => "float8",
        "boolean" => "bool",
        "character" => "bpchar",
        "character varying" => "varchar",
        "time without time zone" => "time",
        "time with time zone" => "timetz",
        "timestamp without time zone" => "timestamp",
        "timestamp with time zone" => "timestamptz",
        name => name,
    }
    .to_string()
}
