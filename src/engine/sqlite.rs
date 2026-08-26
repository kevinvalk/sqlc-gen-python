//! SQLite, which has five storage classes but accepts any declared type name,
//! applying [type affinity] rules to decide how to store a value. Matching
//! sqlc, this keys off the declared name.
//!
//! [type affinity]: https://www.sqlite.org/datatype3.html

use crate::python::Type;

#[cfg(test)]
#[path = "sqlite_spec.rs"]
mod spec;

pub fn scalar_type(data_type: &str) -> Option<Type> {
    let python = match data_type {
        "int" | "integer" | "tinyint" | "smallint" | "mediumint" | "bigint" | "int2" | "int8"
        | "unsignedbigint" => Type::INT,

        "real" | "double" | "doubleprecision" | "float" => Type::FLOAT,

        "decimal" | "numeric" => Type::DECIMAL,

        "bool" | "boolean" => Type::BOOL,

        "text" | "clob" | "character" | "varchar" | "varyingcharacter" | "nchar"
        | "nativecharacter" | "nvarchar" => Type::STR,

        "blob" => Type::BYTES,

        "date" => Type::DATE,
        "datetime" | "timestamp" => Type::DATETIME,

        "json" | "jsonb" | "any" => Type::ANY,

        _ => return None,
    };
    Some(python)
}
