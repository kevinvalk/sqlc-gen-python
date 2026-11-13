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
    assert!(!Backend::SqlAlchemy.defines_errors());
    assert!(Backend::Psycopg.defines_errors());
    assert!(Backend::Sqlite3.defines_errors());
}
