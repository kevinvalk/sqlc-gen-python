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

/// SQLite's rules for the affinity of a declared type, in their order: the
/// first word a type holds decides, wherever it stands (`FLOATING POINT` is
/// an `INT`), and each affinity's class is one Python type.
const AFFINITIES: &[(&str, Type)] = &[
    ("INT", Type::INT),
    ("CHAR", Type::STR),
    ("CLOB", Type::STR),
    ("TEXT", Type::STR),
    ("BLOB", Type::BYTES),
    ("REAL", Type::FLOAT),
    ("FLOA", Type::FLOAT),
    ("DOUB", Type::FLOAT),
];

/// The types of NUMERIC affinity that generated code binds a `bool` for,
/// which `sqlite3` stores as the integer `0` or `1`. sqlc types a comparison
/// as the first.
const BOOLEANS: &[&str] = &["BOOL", "BOOLEAN"];

/// The Python type of a column declared `data_type`, as sqlc spells it: the
/// class of SQLite's affinity for it, by SQLite's own rules, or `None` for
/// NUMERIC affinity (`DECIMAL`, `DATE`, `JSON`), which stores `'2024'` as an
/// integer and `'12.50'` as a real, whatever the column was meant to hold.
pub fn scalar_type(data_type: &str) -> Option<Type> {
    let name = data_type.to_ascii_uppercase();
    let boolean = BOOLEANS.contains(&name.as_str());
    affinity(&name)
        .map(|(_, python)| python.clone())
        .or_else(|| boolean.then_some(Type::BOOL_FROM_INT))
}

/// The rule that decides the affinity of the type `name`, in uppercase.
fn affinity(name: &str) -> Option<&'static (&'static str, Type)> {
    AFFINITIES.iter().find(|(word, _)| name.contains(word))
}
