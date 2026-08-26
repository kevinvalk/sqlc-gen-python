//! Specification for backend selection.

use super::*;

#[test]
fn a_backend_is_named_in_the_configuration() {
    let parse = |json| serde_json::from_str::<Backend>(json).unwrap();
    assert_eq!(parse(r#""sqlalchemy""#), Backend::SqlAlchemy);
}

#[test]
fn selection_dispatches_to_the_backend() {
    let backend = Backend::SqlAlchemy;
    assert_eq!(
        backend.sql("WHERE id = $1", Engine::PostgreSql),
        "WHERE id = :p1"
    );
    assert!(!backend.connection(true).import().is_none());
}
