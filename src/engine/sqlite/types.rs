use crate::python::Type;

#[cfg(test)]
#[path = "types_spec.rs"]
mod spec;

/// SQLite's rules for the affinity of a declared type, in their order: the first rule whose word the type holds
/// decides, wherever it stands (`FLOATING POINT` is an `INT`). The affinity decides which storage class a written value
/// gets, and `sqlite3` and `aiosqlite` return each class as one Python type, so generated code reads back the type it
/// binds.
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

/// The types of NUMERIC affinity that generated code binds a `bool` for, which `sqlite3` stores as the integer `0` or
/// `1`. sqlc types a comparison as the first.
const BOOLEANS: &[&str] = &["BOOL", "BOOLEAN"];

/// `None` for NUMERIC affinity (`DECIMAL`, `DATE`, `JSON`), which stores `'2024'` as an integer and `'12.50'` as a
/// real, whatever the column was meant to hold.
pub fn scalar_type(data_type: &str) -> Option<Type> {
    let name = data_type.to_ascii_uppercase();
    let boolean = BOOLEANS.contains(&name.as_str());
    affinity(&name)
        .map(|(_, python)| python.clone())
        .or_else(|| boolean.then_some(Type::BOOL_FROM_INT))
}

/// The rule that decides the affinity of `name`, which must be uppercase.
fn affinity(name: &str) -> Option<&'static (&'static str, Type)> {
    AFFINITIES.iter().find(|(word, _)| name.contains(word))
}
