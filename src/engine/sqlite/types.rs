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

/// The Python type of a column declared `data_type`, as sqlc spells it: the
/// class of SQLite's affinity for it, by SQLite's own rules, or `None` for
/// NUMERIC affinity (`DECIMAL`, `DATE`, `JSON`), which stores `'2024'` as an
/// integer and `'12.50'` as a real, whatever the column was meant to hold.
///
/// A `BOOL` or `BOOLEAN` is NUMERIC too, but generated code binds a `bool`,
/// which `sqlite3` stores as the integer `0` or `1`.
pub fn scalar_type(data_type: &str) -> Option<Type> {
    let name = data_type.to_ascii_uppercase();
    let has = |part: &str| name.contains(part);
    Some(match name.as_str() {
        _ if has("INT") => Type::INT,
        _ if has("CHAR") || has("CLOB") || has("TEXT") => Type::STR,
        _ if has("BLOB") => Type::BYTES,
        _ if has("REAL") || has("FLOA") || has("DOUB") => Type::FLOAT,
        "BOOL" | "BOOLEAN" => Type::BOOL_FROM_INT,
        _ => return None,
    })
}
