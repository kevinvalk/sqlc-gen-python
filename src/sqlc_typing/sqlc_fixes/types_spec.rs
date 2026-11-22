use super::*;

#[test]
fn sqls_standard_names_are_guesses() {
    // sqlc's static analyser writes a declared or cast type by PostgreSQL's own name (`pg_catalog.int8`,
    // `timestamptz`), and SQL's standard name only for a type it took off a function's signature: the overload it
    // guessed. So are a bare `int` (its type for any arithmetic) and a type it has no name for.
    for guess in [
        "",
        "any",
        "anyelement",
        "anyarray",
        "int",
        "smallint",
        "bigint",
        "real",
        "double precision",
        "numeric",
        "boolean",
        "character",
        "character varying",
        "interval",
        "time with time zone",
        "time without time zone",
        "timestamp with time zone",
        "timestamp without time zone",
    ] {
        assert!(postgres_guess(guess), "{guess:?}");
    }
    // So only an override writes one for the type it is the standard's name of
    for (standard, typname) in [
        ("int", "int4"),
        ("smallint", "int2"),
        ("bigint", "int8"),
        ("real", "float4"),
        ("double precision", "float8"),
        ("boolean", "bool"),
        ("character", "bpchar"),
        ("character varying", "varchar"),
        ("time with time zone", "timetz"),
        ("time without time zone", "time"),
        ("timestamp with time zone", "timestamptz"),
        ("timestamp without time zone", "timestamp"),
    ] {
        assert!(postgres_guess(standard), "{standard}");
        assert_eq!(Engine::PostgreSql.type_name(standard), typname);
    }
    for declared in [
        "pg_catalog.int8",
        "pg_catalog.numeric",
        "pg_catalog.interval",
        "pg_catalog.timestamptz",
        "int8",
        "timestamptz",
        "text",
        "date",
        "integer",
        "smallserial",
        "serial",
        "bigserial",
    ] {
        assert!(!postgres_guess(declared), "{declared}");
    }
}
