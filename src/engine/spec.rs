//! Specification for engine selection and dispatch.

use super::*;

#[test]
fn every_supported_engine_parses() {
    assert_eq!(Engine::parse("postgresql").unwrap(), Engine::PostgreSql);
    assert_eq!(Engine::parse("mysql").unwrap(), Engine::MySql);
    assert_eq!(Engine::parse("sqlite").unwrap(), Engine::Sqlite);
}

#[test]
fn an_unsupported_engine_is_named_in_the_error() {
    let error = Engine::parse("oracle").unwrap_err();
    assert!(error.to_string().contains("oracle"), "{error}");
}

#[test]
fn selection_dispatches_to_the_engines_own_table() {
    // Each engine spells its integer and text types differently; the dispatch
    // is what makes the right table answer.
    for (engine, integer, text) in [
        (Engine::PostgreSql, "bigserial", "citext"),
        (Engine::MySql, "bigint unsigned", "longtext"),
        (Engine::Sqlite, "unsignedbigint", "nvarchar"),
    ] {
        assert_eq!(engine.scalar_type(integer), Some(Type::INT));
        assert_eq!(engine.scalar_type(text), Some(Type::STR));
    }
}
