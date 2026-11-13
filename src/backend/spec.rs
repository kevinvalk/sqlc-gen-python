//! Specification for choosing a backend.

use super::*;

#[test]
fn each_backend_runs_its_engines() {
    use Engine::{PostgreSql, Sqlite};
    for (backend, engines) in [
        (Backend::SqlAlchemy, &[PostgreSql, Sqlite][..]),
        (Backend::Psycopg, &[PostgreSql]),
        (Backend::Sqlite3, &[Sqlite]),
    ] {
        for engine in [PostgreSql, Sqlite] {
            assert_eq!(
                backend.runs(engine),
                engines.contains(&engine),
                "{backend:?}"
            );
        }
    }
}

#[test]
fn only_a_driver_without_its_own_defines_the_errors_of_a_one() {
    assert!(Backend::SqlAlchemy.errors(true).is_empty());
    assert_eq!(
        Backend::Psycopg.errors(true),
        ["MultipleRowsError", "NoRowError"]
    );
    // No `:one` raises without a row: only a second row raises.
    assert_eq!(Backend::Sqlite3.errors(false), ["MultipleRowsError"]);
}

#[test]
fn a_type_the_drivers_disagree_on_is_the_backends_to_decide() {
    use crate::python::Type;
    let annotation = |backend: Backend, data_type: &str| {
        (backend.scalar_type(Engine::PostgreSql, data_type))
            .map(|ty| crate::python::python_source(&ty.annotation(crate::python::Scope::Queries)))
    };
    // A range is each library's class, SQLAlchemy's whatever the driver.
    assert_eq!(
        annotation(Backend::Psycopg, "int4range").as_deref(),
        Some("Range[int]")
    );
    assert_eq!(
        annotation(Backend::SqlAlchemy, "pg_catalog.daterange").as_deref(),
        Some("postgresql.Range[dt.date]")
    );
    // psycopg returns a point's text; asyncpg, under SQLAlchemy, an object.
    assert_eq!(
        Backend::Psycopg.scalar_type(Engine::PostgreSql, "point"),
        Some(Type::STR)
    );
    assert_eq!(
        Backend::SqlAlchemy.scalar_type(Engine::PostgreSql, "point"),
        None
    );
    assert_eq!(
        Backend::Sqlite3.scalar_type(Engine::Sqlite, "int4range"),
        None
    );
}
