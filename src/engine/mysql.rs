//! MySQL, which reports width and signedness separately; neither changes the
//! Python type, since Python integers are arbitrary precision.

use crate::python::Type;

#[cfg(test)]
#[path = "mysql_spec.rs"]
mod spec;

pub fn scalar_type(data_type: &str) -> Option<Type> {
    let python = match data_type {
        "tinyint" | "smallint" | "mediumint" | "int" | "integer" | "bigint" | "bigint signed"
        | "bigint unsigned" | "year" => Type::INT,

        "float" | "double" | "double precision" | "real" => Type::FLOAT,

        "decimal" | "dec" | "fixed" | "numeric" => Type::DECIMAL,

        "bool" | "boolean" => Type::BOOL,

        "char" | "varchar" | "tinytext" | "text" | "mediumtext" | "longtext" | "enum" => Type::STR,

        "binary" | "varbinary" | "tinyblob" | "blob" | "mediumblob" | "longblob" => Type::BYTES,

        "date" => Type::DATE,
        // MySQL's TIME is a signed duration, not a wall clock, and may exceed
        // 24 hours, so it is not `datetime.time`.
        "time" => Type::TIMEDELTA,
        "datetime" | "timestamp" => Type::DATETIME,

        "json" | "any" => Type::ANY,

        _ => return None,
    };
    Some(python)
}
