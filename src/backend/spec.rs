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
            assert_eq!(backend.runs(engine), engines.contains(&engine), "{backend:?}");
        }
    }
}

#[test]
fn a_backends_coroutines_take_the_connection_of_its_async_library() {
    let connection = |backend: Backend, asynchronous| crate::python::unparse(&backend.connection(asynchronous));
    for (backend, sync, asynchronous) in [
        (Backend::SqlAlchemy, "sqlalchemy.Connection", "AsyncConnection"),
        (
            Backend::Psycopg,
            "psycopg.Connection[Any]",
            "psycopg.AsyncConnection[Any]",
        ),
        (Backend::Sqlite3, "sqlite3.Connection", "aiosqlite.Connection"),
        // The module's own name for `pymysql.Connection`, see `dbapi::prelude`
        (Backend::Pymysql, "_Connection", "asyncmy.Connection"),
    ] {
        assert_eq!(connection(backend, false), sync);
        assert_eq!(connection(backend, true), asynchronous);
    }
}

/// A type only joins a map with driver evidence (`AGENTS.md`), which the runtime proof gets by reading a column of it
/// (`tests/python/typemap_psycopg.py`).
#[test]
fn every_type_psycopg_alone_maps_has_a_column_the_runtime_proof_reads() {
    // We read where cargo runs the test from and not where it was built, as a reused `target/` may come from another
    // copy of the tree.
    let path =
        std::path::Path::new(&std::env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("tests/sqlc/examples/pgtext.json");
    let request: crate::plugin::GenerateRequest =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let columns: Vec<String> = (request.catalog.iter())
        .flat_map(|catalog| &catalog.schemas)
        .flat_map(|schema| &schema.tables)
        .flat_map(|table| &table.columns)
        .map(|column| Engine::PostgreSql.type_name(&column.data_type()))
        .collect();
    let mapped = (psycopg::TEXT.iter().copied()).chain(RANGES.iter().map(|(range, ..)| *range));
    let missing: Vec<&str> = mapped
        .filter(|name| !columns.iter().any(|column| column == name))
        .collect();
    assert!(missing.is_empty(), "no column of {missing:?}");
}

#[test]
fn a_type_the_drivers_disagree_on_is_the_backends_to_decide() {
    use crate::python::Type;
    let annotation = |backend: Backend, data_type: &str| {
        (backend.scalar_type(Engine::PostgreSql, data_type))
            .map(|ty| crate::python::unparse(&ty.annotation(crate::python::Scope::Queries)))
    };
    // A range is each library's own class, and SQLAlchemy's whatever the driver
    assert_eq!(annotation(Backend::Psycopg, "int4range").as_deref(), Some("Range[int]"));
    assert_eq!(
        annotation(Backend::SqlAlchemy, "daterange").as_deref(),
        Some("postgresql.Range[dt.date]")
    );
    // psycopg returns a point's text, while asyncpg under SQLAlchemy returns an object
    assert_eq!(
        Backend::Psycopg.scalar_type(Engine::PostgreSql, "point"),
        Some(Type::STR)
    );
    assert_eq!(Backend::SqlAlchemy.scalar_type(Engine::PostgreSql, "point"), None);
    assert_eq!(Backend::Sqlite3.scalar_type(Engine::Sqlite, "int4range"), None);
}

#[test]
fn sqlalchemy_streams_on_a_server_side_cursor_sync_and_async() {
    use crate::api::fixtures::authors::{named, query, resolve};
    use crate::api::fixtures::column;
    use crate::api::queries::Command;

    // We bind the stream to a name no parameter has, where `result` is a column's as often as not
    let many = resolve(crate::plugin::Query {
        columns: vec![column("bio", "text", false)],
        params: vec![named(1, "result", "text", true)],
        ..query(":many", "SELECT bio FROM authors WHERE name = $1")
    });
    let Command::Many(shape) = &many.command else {
        panic!("not a :many")
    };
    let stream = |asynchronous| {
        let body = Backend::SqlAlchemy.stream(&many, ("stream_get_author", shape), asynchronous);
        crate::python::unparse_stmts(&body)
    };
    assert_eq!(
        stream(false),
        "with connection.execute(_GET_AUTHOR, {\"result\": result}, execution_options={\"stream_results\": True}) as cur:\n    \
         yield (row[0] for row in cur)"
    );
    assert_eq!(
        stream(true),
        "async with connection.stream(_GET_AUTHOR, {\"result\": result}) as cur:\n    \
         yield (row[0] async for row in cur)"
    );
}
