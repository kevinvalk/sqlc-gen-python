//! Specification for choosing the engine.

use super::*;

#[test]
fn postgresql_and_sqlite_are_supported() {
    let request = |engine: &str| plugin::GenerateRequest {
        settings: Some(plugin::Settings {
            engine: engine.into(),
            ..plugin::Settings::default()
        }),
        ..plugin::GenerateRequest::default()
    };
    for engine in [Engine::PostgreSql, Engine::Sqlite] {
        assert_eq!(Engine::of(&request(engine.name())).unwrap(), engine);
    }
    // MySQL has no driver evidence behind a type map yet.
    for engine in ["mysql", "oracle"] {
        assert_eq!(
            Engine::of(&request(engine)).unwrap_err().to_string(),
            format!(r#"engine "{engine}" is not supported yet; only postgresql and sqlite are"#)
        );
    }
}
