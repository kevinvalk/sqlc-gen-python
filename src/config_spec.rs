//! Specification for the plugin configuration surface.

use super::*;

fn parse(json: &str) -> Result<Config> {
    let request = plugin::GenerateRequest {
        plugin_options: json.as_bytes().to_vec(),
        ..plugin::GenerateRequest::default()
    };
    Config::from_request(&request)
}

#[test]
fn the_backend_and_async_must_be_named() {
    // sqlc sends empty bytes for a codegen entry without `options:`.
    for (options, missing) in [
        ("", "backend"),
        ("{}", "backend"),
        (r#"{"async": false}"#, "backend"),
        (r#"{"backend": "psycopg"}"#, "async"),
    ] {
        let error = format!("{:#}", parse(options).unwrap_err());
        assert!(
            error.contains(&format!("missing field `{missing}`")),
            "{options}: {error}"
        );
    }
    for (name, backend) in [
        ("sqlalchemy", Backend::SqlAlchemy),
        ("psycopg", Backend::Psycopg),
        ("sqlite3", Backend::Sqlite3),
    ] {
        for asynchronous in [false, true] {
            let options = format!(r#"{{"backend": "{name}", "async": {asynchronous}}}"#);
            let config = parse(&options).unwrap();
            assert_eq!(
                (config.backend, config.asynchronous),
                (backend, asynchronous)
            );
        }
    }
    assert!(parse(r#"{"backend": "psycopg2", "async": false}"#).is_err());
}

#[test]
fn reads_nested_groups() {
    let options = parse(
        r#"{
            "backend": "sqlalchemy",
            "async": false,
            "omit_sqlc_version": true,
            "models": { "exact_table_names": true, "inflection_exclude": ["campus"] }
        }"#,
    )
    .unwrap();

    assert!(options.omit_sqlc_version);
    assert!(options.models.exact_table_names);
    assert_eq!(options.models.inflection_exclude, ["campus"]);
}

#[test]
fn rejects_unknown_keys() {
    // Neither the old plugins' `emit_*` flags nor options this plugin removed
    // (docs/python-output.md §7) may be quietly ignored.
    for stale in [
        r#"{"backend": "psycopg", "async": false, "emit_sync_querier": true}"#,
        r#"{"backend": "psycopg", "async": false, "package": "db"}"#,
        r#"{"backend": "psycopg", "async": false, "queries": {"many": "iterator"}}"#,
        r#"{"backend": "psycopg", "async": false, "models": {"style": "pydantic"}}"#,
    ] {
        let error = parse(stale).unwrap_err();
        assert!(error.to_string().contains("plugin options"), "{error}");
    }
}

#[test]
fn out_must_end_in_a_name_the_package_can_be_imported_by() {
    let with_out = |out: &str| {
        Config::from_request(&plugin::GenerateRequest {
            settings: Some(plugin::Settings {
                codegen: Some(plugin::Codegen {
                    out: out.into(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            plugin_options: br#"{"backend": "psycopg", "async": false}"#.to_vec(),
            ..Default::default()
        })
    };
    for out in ["app/db", "db/", "invoicing_db"] {
        with_out(out).unwrap();
    }
    for out in ["app/my-db", "app/2db", "app/Db"] {
        let error = format!("{:#}", with_out(out).unwrap_err());
        assert!(
            error.contains("is not a lowercase Python package name"),
            "{out}: {error}"
        );
    }
}
