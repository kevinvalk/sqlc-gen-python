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
        ("pymysql", Backend::Pymysql),
    ] {
        for asynchronous in [false, true] {
            let options = format!(r#"{{"backend": "{name}", "async": {asynchronous}}}"#);
            let config = parse(&options).unwrap();
            assert_eq!((config.backend, config.asynchronous), (backend, asynchronous));
        }
    }
    assert!(parse(r#"{"backend": "psycopg2", "async": false}"#).is_err());
}

#[test]
fn an_option_error_points_at_the_option_not_into_the_json_sqlc_sent() {
    let error = format!(
        "{:#}",
        parse(r#"{"backend": "psycopg", "async": false, "max_params": "3"}"#).unwrap_err()
    );
    assert_eq!(
        error,
        r#"reading plugin options: invalid type: string "3", expected usize"#
    );
}

#[test]
fn an_override_names_exactly_one_of_a_type_and_a_column() {
    for rule in [
        r#"{"python_type": "str"}"#,
        r#"{"db_type": "x", "column": "t.c", "python_type": "str"}"#,
    ] {
        let options = format!(r#"{{"backend": "sqlalchemy", "async": false, "overrides": [{rule}]}}"#);
        let error = format!("{:#}", parse(&options).unwrap_err());
        assert!(error.contains("exactly one of db_type and column"), "{rule}: {error}");
    }
}

#[test]
fn rejects_unknown_keys() {
    // Neither the old plugins' `emit_*` flags nor options we removed may be quietly ignored
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
    let with_out = |out: &str, options: &str| {
        Config::from_request(&plugin::GenerateRequest {
            settings: Some(plugin::Settings {
                codegen: Some(plugin::Codegen {
                    out: out.into(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            plugin_options: format!(r#"{{"backend": "psycopg", "async": false{options}}}"#).into_bytes(),
            ..Default::default()
        })
    };
    for out in ["app/db", "db/", "invoicing_db"] {
        with_out(out, "").unwrap();
    }
    for out in ["app/my-db", "app/2db", "app/Db"] {
        let error = format!("{:#}", with_out(out, "").unwrap_err());
        assert!(
            error.contains("is not a lowercase Python package name"),
            "{out}: {error}"
        );
    }
    // Query modules that read their models from a module the user named import nothing relatively
    with_out("app/my-db", r#", "models": {"module": "app.models"}"#).unwrap();
}

#[test]
fn the_models_module_is_a_dotted_module_name() {
    for module in ["app.models", "models", "gateway.db.models"] {
        parse(&format!(
            r#"{{"backend": "psycopg", "async": false, "models": {{"module": "{module}"}}}}"#
        ))
        .unwrap();
    }
    for module in ["", "app..models", ".models", "App.models", "app.my-models"] {
        let options = format!(r#"{{"backend": "psycopg", "async": false, "models": {{"module": "{module}"}}}}"#);
        let error = format!("{:#}", parse(&options).unwrap_err());
        assert!(error.contains("is no dotted name"), "{module}: {error}");
    }
}

#[test]
fn an_entry_writes_the_models_or_reads_them_from_another() {
    let error = parse(r#"{"backend": "psycopg", "async": false, "models": {"only": true, "module": "app.models"}}"#)
        .unwrap_err();
    assert!(format!("{error:#}").contains("set one"), "{error:#}");
}

#[test]
fn a_rename_names_a_public_python_name() {
    let rename = |name: &str| {
        parse(&format!(
            r#"{{"backend": "psycopg", "async": false, "rename": {{"id": "{name}"}}}}"#
        ))
    };
    assert!(rename("identifier").is_ok());
    for bad in ["_id", "class", "1st", "a b"] {
        let error = format!("{:#}", rename(bad).unwrap_err());
        assert!(error.contains("is no public Python name"), "{bad}: {error}");
    }
}

#[test]
fn records_are_dataclasses_or_pydantic_models() {
    let records = |value: &str| {
        parse(&format!(
            r#"{{"backend": "psycopg", "async": false, "records": "{value}"}}"#
        ))
    };
    assert_eq!(records("dataclass").unwrap().records, Records::Dataclass);
    assert_eq!(records("pydantic").unwrap().records, Records::Pydantic);
    assert!(records("attrs").is_err());
    assert_eq!(
        parse(r#"{"backend": "psycopg", "async": false}"#).unwrap().records,
        Records::Dataclass
    );
}

#[test]
fn an_entry_that_shares_its_models_omits_none() {
    for models in [
        r#"{"only": true, "omit_unused": true}"#,
        r#"{"module": "app.models", "omit_unused": true}"#,
    ] {
        let error = parse(&format!(
            r#"{{"backend": "psycopg", "async": false, "models": {models}}}"#
        ))
        .unwrap_err();
        assert!(format!("{error:#}").contains("drop it here"), "{error:#}");
    }
}
