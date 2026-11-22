use crate::plugin;
use crate::python::Type;

#[cfg(test)]
#[path = "types_spec.rs"]
mod spec;

/// What PyMySQL and asyncmy return over MySQL's text protocol, which SQLAlchemy hands through unchanged, keyed by the
/// first word of the type sqlc reports (`bigint unsigned` is a `bigint`). sqlc names a type as MySQL does, whatever the
/// schema spells: `INTEGER` is `int`, `REAL` is `double`, `NUMERIC` is `decimal` and `BOOL` is `tinyint`.
const TYPES: &[(&str, Type)] = &[
    // What sqlc types a comparison, which MySQL returns as an integer
    ("bool", Type::BOOL_FROM_INT),
    ("tinyint", Type::INT),
    ("smallint", Type::INT),
    ("mediumint", Type::INT),
    ("int", Type::INT),
    // What sqlc types the parameter of a `LIMIT` or an `OFFSET`
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
    // The drivers return a JSON document's text, undecoded
    ("json", Type::STR),
    ("binary", Type::BYTES),
    ("varbinary", Type::BYTES),
    ("tinyblob", Type::BYTES),
    ("blob", Type::BYTES),
    ("mediumblob", Type::BYTES),
    ("longblob", Type::BYTES),
    ("bit", Type::BYTES),
    ("date", Type::DATE),
    // Naive, as MySQL stores no time zone and converts a `TIMESTAMP` to the session's
    ("datetime", Type::DATETIME),
    ("timestamp", Type::DATETIME),
    // A duration, which may be negative or over a day: `'-01:02:03'`
    ("time", Type::TIMEDELTA),
];

/// `None` when nobody has probed what the drivers return for the column's type.
pub fn scalar_type(column: &plugin::Column) -> Option<Type> {
    let data_type = column.data_type().to_ascii_lowercase();
    let name = data_type.split_whitespace().next().unwrap_or_default();

    // `BOOL` is `TINYINT(1)`, stored as an integer, which by convention (and sqlc-gen-go's) is a boolean
    if name == "tinyint" && column.length == 1 {
        return Some(Type::BOOL_FROM_INT);
    }
    (TYPES.iter())
        .find(|(known, _)| *known == name)
        .map(|(_, python)| python.clone())
}
