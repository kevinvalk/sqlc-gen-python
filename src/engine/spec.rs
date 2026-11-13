//! Specification for choosing the engine.

use super::*;

#[test]
fn postgresql_mysql_and_sqlite_are_supported() {
    let request = |engine: &str| plugin::GenerateRequest {
        settings: Some(plugin::Settings {
            engine: engine.into(),
            ..plugin::Settings::default()
        }),
        ..plugin::GenerateRequest::default()
    };
    for engine in [Engine::PostgreSql, Engine::Sqlite, Engine::MySql] {
        assert_eq!(Engine::of(&request(engine.name())).unwrap(), engine);
    }
    for engine in ["oracle", ""] {
        assert_eq!(
            Engine::of(&request(engine)).unwrap_err().to_string(),
            format!(r#"engine "{engine}" is not supported; only postgresql, mysql and sqlite are"#)
        );
    }
}
