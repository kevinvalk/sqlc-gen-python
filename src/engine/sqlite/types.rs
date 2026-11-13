//! Mapping SQLite's declared column types to Python types.
//!
//! SQLite stores a value as one of five classes, and `sqlite3` and
//! `aiosqlite` return each as one Python type (`NULL` `None`, `INTEGER`
//! `int`, `REAL` `float`, `TEXT` `str`, `BLOB` `bytes`). A column's declared
//! type decides which class a value written to it is stored as (its
//! affinity), so the type is what generated code, which binds that type,
//! reads back.

use crate::python::Type;

#[cfg(test)]
#[path = "types_spec.rs"]
mod spec;

/// The Python type of a column declared `data_type`, as sqlc spells it, or
/// `None`.
///
/// Not mapped: `NUMERIC` and `DECIMAL`, whose affinity stores `12.50` as the
/// float `12.5`, and any type of no affinity sqlc's Go codegen knows.
pub fn scalar_type(data_type: &str) -> Option<Type> {
    let name = data_type.to_ascii_lowercase();
    let strings = [
        "character",
        "varchar",
        "varyingcharacter",
        "nchar",
        "nativecharacter",
        "nvarchar",
        "text",
        "clob",
    ];
    Some(match name.as_str() {
        "int" | "integer" | "tinyint" | "smallint" | "mediumint" | "bigint" | "unsignedbigint"
        | "int2" | "int8" => Type::INT,
        "real" | "double" | "doubleprecision" | "float" => Type::FLOAT,
        "blob" => Type::BYTES,
        // Stored as the integer `0` or `1`.
        "bool" | "boolean" => Type::BOOL_FROM_INT,
        // SQLite has no date or JSON type: the text written is read back.
        "date" | "datetime" | "timestamp" | "json" | "jsonb" => Type::STR,
        name if strings.iter().any(|prefix| name.starts_with(prefix)) => Type::STR,
        _ => return None,
    })
}

/// SQLite has no arrays.
pub fn array_problem(_data_type: &str) -> Option<&'static str> {
    Some("SQLite does not have")
}

/// Whether `data_type` is sqlc's guess rather than a type it read: `any` for
/// an expression it could not type, or nothing.
pub fn is_guess(data_type: &str, _database: bool) -> bool {
    ["", "any"].contains(&data_type.to_ascii_lowercase().as_str())
}
