//! PostgreSQL, whose catalog reports some types both bare and
//! `pg_catalog`-qualified depending on how the column was declared.

use crate::python::Type;

#[cfg(test)]
#[path = "postgresql_spec.rs"]
mod spec;

pub fn scalar_type(data_type: &str) -> Option<Type> {
    let python = match data_type {
        // Python has one integer type, so width only matters to the database.
        "smallint" | "int2" | "pg_catalog.int2" | "smallserial" | "serial2"
        | "pg_catalog.serial2" | "integer" | "int" | "int4" | "pg_catalog.int4" | "serial"
        | "serial4" | "pg_catalog.serial4" | "bigint" | "int8" | "pg_catalog.int8"
        | "bigserial" | "serial8" | "pg_catalog.serial8" | "oid" | "cid" | "tid" | "xid"
        | "xid8" => Type::INT,

        "real" | "float4" | "pg_catalog.float4" | "double precision" | "float8"
        | "pg_catalog.float8" | "float" => Type::FLOAT,

        // `numeric` is exact, so a float would silently lose precision, and
        // `money` is fixed-point for the same reason.
        "numeric" | "pg_catalog.numeric" | "decimal" | "money" => Type::DECIMAL,

        "boolean" | "bool" | "pg_catalog.bool" => Type::BOOL,

        "text" | "varchar" | "pg_catalog.varchar" | "character varying" | "bpchar"
        | "pg_catalog.bpchar" | "character" | "char" | "name" | "string" | "citext" => Type::STR,

        "bytea" | "pg_catalog.bytea" | "blob" => Type::BYTES,

        "date" => Type::DATE,
        "time"
        | "pg_catalog.time"
        | "timetz"
        | "pg_catalog.timetz"
        | "time with time zone"
        | "time without time zone" => Type::TIME,
        "timestamp"
        | "pg_catalog.timestamp"
        | "timestamptz"
        | "pg_catalog.timestamptz"
        | "timestamp with time zone"
        | "timestamp without time zone" => Type::DATETIME,
        "interval" | "pg_catalog.interval" => Type::TIMEDELTA,

        "uuid" => Type::UUID,

        // The driver decodes JSON before we see it, and its shape is whatever
        // the query produced.
        "json" | "pg_catalog.json" | "jsonb" | "pg_catalog.jsonb" => Type::ANY,

        // psycopg can adapt network types to `ipaddress` objects but does not
        // by default, so the faithful mapping is the text form. Bit strings,
        // label trees and geometric types arrive as text too.
        // See https://www.psycopg.org/docs/extras.html#adapt-network
        "inet" | "cidr" | "macaddr" | "macaddr8" | "bit" | "pg_catalog.bit" | "varbit"
        | "pg_catalog.varbit" | "bit varying" | "ltree" | "lquery" | "ltxtquery" | "point"
        | "line" | "lseg" | "box" | "path" | "polygon" | "circle" => Type::STR,

        // Ranges and extension types are driver-specific objects.
        "int4range" | "int8range" | "numrange" | "tsrange" | "tstzrange" | "daterange"
        | "int4multirange" | "int8multirange" | "nummultirange" | "tsmultirange"
        | "tstzmultirange" | "datemultirange" | "hstore" | "vector" => Type::ANY,

        // What a function returning nothing reports.
        "void" => Type::NONE,

        "any" | "anyelement" | "anyarray" => Type::ANY,

        _ => return None,
    };
    Some(python)
}
