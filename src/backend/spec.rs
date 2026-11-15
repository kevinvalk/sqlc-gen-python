//! Specification for choosing a backend.

use super::*;

#[test]
fn each_backend_runs_its_engines() {
    use Engine::{MySql, PostgreSql, Sqlite};
    for (backend, engines) in [
        (Backend::SqlAlchemy, &[PostgreSql, MySql, Sqlite][..]),
        (Backend::Psycopg, &[PostgreSql]),
        (Backend::Sqlite3, &[Sqlite]),
        (Backend::Pymysql, &[MySql]),
    ] {
        for engine in [PostgreSql, MySql, Sqlite] {
            assert_eq!(
                backend.runs(engine),
                engines.contains(&engine),
                "{backend:?}"
            );
        }
    }
}

#[test]
fn a_backends_coroutines_take_the_connection_of_its_async_library() {
    let connection =
        |backend: Backend, asynchronous| crate::python::unparse(&backend.connection(asynchronous));
    for (backend, sync, asynchronous) in [
        (
            Backend::SqlAlchemy,
            "sqlalchemy.Connection",
            "AsyncConnection",
        ),
        (
            Backend::Psycopg,
            "psycopg.Connection[Any]",
            "psycopg.AsyncConnection[Any]",
        ),
        (
            Backend::Sqlite3,
            "sqlite3.Connection",
            "aiosqlite.Connection",
        ),
        (Backend::Pymysql, "pymysql.Connection", "asyncmy.Connection"),
    ] {
        assert_eq!(connection(backend, false), sync);
        assert_eq!(connection(backend, true), asynchronous);
    }
}

#[test]
fn only_a_driver_without_its_own_defines_the_errors_of_a_one() {
    assert!(Backend::SqlAlchemy.errors().is_empty());
    assert_eq!(
        Backend::Psycopg.errors(),
        [MULTIPLE_ROWS_ERROR, NO_ROW_ERROR]
    );
}

#[test]
fn a_type_the_drivers_disagree_on_is_the_backends_to_decide() {
    use crate::python::Type;
    let annotation = |backend: Backend, data_type: &str| {
        (backend.scalar_type(Engine::PostgreSql, data_type))
            .map(|ty| crate::python::unparse(&ty.annotation(crate::python::Scope::Queries)))
    };
    // A range is each library's class, SQLAlchemy's whatever the driver.
    assert_eq!(
        annotation(Backend::Psycopg, "int4range").as_deref(),
        Some("Range[int]")
    );
    assert_eq!(
        annotation(Backend::SqlAlchemy, "daterange").as_deref(),
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
