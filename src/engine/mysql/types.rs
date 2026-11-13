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

/// The Python type of a column of `column`'s type, or `None` if nobody has
/// probed what the drivers return for it.
pub fn scalar_type(column: &plugin::Column) -> Option<Type> {
    let data_type = column.data_type().to_ascii_lowercase();
    // `bigint unsigned` and `double precision` are their first word's type.
    let name = data_type.split_whitespace().next().unwrap_or_default();
    Some(match name {
        // `BOOL` is `TINYINT(1)`, stored as an integer: by convention, and
        // sqlc-gen-go's, a boolean.
        "bool" | "boolean" => Type::BOOL_FROM_INT,
        "tinyint" if column.length == 1 => Type::BOOL_FROM_INT,
        "tinyint" | "smallint" | "mediumint" | "int" | "integer" | "bigint" | "year" => Type::INT,
        "float" | "double" | "real" => Type::FLOAT,
        "decimal" | "numeric" | "dec" | "fixed" => Type::DECIMAL,
        // The drivers return a JSON document's text, undecoded.
        "char" | "varchar" | "tinytext" | "text" | "mediumtext" | "longtext" | "json" => Type::STR,
        "binary" | "varbinary" | "tinyblob" | "blob" | "mediumblob" | "longblob" | "bit" => {
            Type::BYTES
        }
        "date" => Type::DATE,
        // Naive: MySQL stores no time zone, and converts a `TIMESTAMP` to the
        // session's.
        "datetime" | "timestamp" => Type::DATETIME,
        // A duration, which may be negative or over a day: `'-01:02:03'`.
        "time" => Type::TIMEDELTA,
        _ => return None,
    })
}

/// Whether `data_type` is sqlc's guess rather than a type it read: `any` for
/// an expression it could not type, or nothing.
pub fn is_guess(data_type: &str, _database: bool) -> bool {
    ["", "any"].contains(&data_type.to_ascii_lowercase().as_str())
}

/// Why a column of an enum of `labels` is no enum, if it is not: sqlc reports
/// a `SET` as an `ENUM` of its members, but a `SET` holds several, as
/// `'x,y'`, which no member is. Its length tells them apart: an `ENUM`'s is
/// its longest label's, a `SET`'s all of them joined by commas.
pub fn enum_problem(column: &plugin::Column, labels: &[String]) -> Option<&'static str> {
    let longest = labels.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    let joined =
        labels.iter().map(|l| l.chars().count()).sum::<usize>() + labels.len().saturating_sub(1);
    let length = usize::try_from(column.length).unwrap_or(0);
    (labels.len() > 1 && length == joined && length > longest)
        .then_some("is a SET, which holds several of its members; store them otherwise, such as in a JSON column")
}
