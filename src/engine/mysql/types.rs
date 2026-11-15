//! Mapping MySQL column types to Python types.
//!
//! Every entry is what PyMySQL and asyncmy return over MySQL's text protocol,
//! and SQLAlchemy hands through unchanged from them
//! (`docs/python-output.md` §5, "MySQL"). sqlc reports a type by its lowercase
//! name, with its length and signedness beside it.

use crate::plugin;
use crate::python::Type;

#[cfg(test)]
#[path = "types_spec.rs"]
mod spec;

/// Each type sqlc reports of a MySQL column or expression, by its first word
/// (`bigint unsigned` is a `bigint`), and what the drivers return for it.
/// sqlc names a type as MySQL does, whatever the schema spells: `INTEGER` is
/// `int`, `REAL` `double`, `NUMERIC` `decimal`, and `BOOL` `tinyint`.
const TYPES: &[(&str, Type)] = &[
    // What sqlc types a comparison, which MySQL returns as an integer.
    ("bool", Type::BOOL_FROM_INT),
    ("tinyint", Type::INT),
    ("smallint", Type::INT),
    ("mediumint", Type::INT),
    ("int", Type::INT),
    // What sqlc types the parameter of a `LIMIT` or an `OFFSET`.
    ("integer", Type::INT),
    ("bigint", Type::INT),
    ("year", Type::INT),
    ("float", Type::FLOAT),
    ("double", Type::FLOAT),
    ("decimal", Type::DECIMAL),
    ("char", Type::STR),
    ("varchar", Type::STR),
    ("tinytext", Type::STR),
    ("text", Type::STR),
    ("mediumtext", Type::STR),
    ("longtext", Type::STR),
    // The drivers return a JSON document's text, undecoded.
    ("json", Type::STR),
    ("binary", Type::BYTES),
    ("varbinary", Type::BYTES),
    ("tinyblob", Type::BYTES),
    ("blob", Type::BYTES),
    ("mediumblob", Type::BYTES),
    ("longblob", Type::BYTES),
    ("bit", Type::BYTES),
    ("date", Type::DATE),
    // Naive: MySQL stores no time zone, and converts a `TIMESTAMP` to the
    // session's.
    ("datetime", Type::DATETIME),
    ("timestamp", Type::DATETIME),
    // A duration, which may be negative or over a day: `'-01:02:03'`.
    ("time", Type::TIMEDELTA),
];

/// The Python type of a column of `column`'s type, or `None` if nobody has
/// probed what the drivers return for it.
pub fn scalar_type(column: &plugin::Column) -> Option<Type> {
    let data_type = column.data_type().to_ascii_lowercase();
    let name = data_type.split_whitespace().next().unwrap_or_default();
    // `BOOL` is `TINYINT(1)`, stored as an integer: by convention, and
    // sqlc-gen-go's, a boolean.
    if name == "tinyint" && column.length == 1 {
        return Some(Type::BOOL_FROM_INT);
    }
    (TYPES.iter())
        .find(|(known, _)| *known == name)
        .map(|(_, python)| python.clone())
}

/// Why a column of an enum of `labels` is no enum, if it is not: sqlc reports
/// a `SET` as an `ENUM` of its members, but a `SET` holds several, as
/// `'x,y'`, which no member is. Its length, in bytes, tells them apart: an
/// `ENUM`'s is its longest label's, a `SET`'s all of them joined by commas.
/// Of one label, the two are one length.
pub fn enum_problem(column: &plugin::Column, labels: &[String]) -> Option<&'static str> {
    let longest = labels.iter().map(String::len).max().unwrap_or(0);
    let joined = labels.iter().map(String::len).sum::<usize>() + labels.len().saturating_sub(1);
    let length = usize::try_from(column.length).unwrap_or(0);
    match labels.len() {
        1 => Some(
            "has one member, which sqlc reports alike for an ENUM and a SET; give it a second, \
             or store it otherwise",
        ),
        _ if length == joined && length > longest => Some(
            "is a SET, which holds several of its members; store them otherwise, such as in a \
             JSON column",
        ),
        _ => None,
    }
}
