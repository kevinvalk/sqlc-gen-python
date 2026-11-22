use super::*;
use crate::api::fixtures::authors::{of_authors, param, query};
use crate::api::fixtures::{column, error_of};

#[test]
fn a_missing_parameter_type_cannot_turn_a_scalar_bind_into_a_slice() {
    let scalar = plugin::Column {
        is_named_param: true,
        ..column("ids", "any", false)
    };
    let slice = plugin::Column {
        is_named_param: true,
        is_sqlc_slice: true,
        ..column("ids", "int", true)
    };
    for (first, second) in [(&scalar, &slice), (&slice, &scalar)] {
        let query = plugin::Query {
            params: vec![param(1, first.clone()), param(2, second.clone())],
            ..Default::default()
        };
        assert!(error_of(parameter_columns(Engine::MySql, &query)).contains("both a scalar and a sqlc.slice"));
    }
}

#[test]
fn postgres_expression_names_have_no_special_meaning_on_mysql_or_sqlite() {
    let query = plugin::Query {
        params: vec![param(
            1,
            plugin::Column {
                is_named_param: true,
                ..column("pg_catalog_id", "integer", true)
            },
        )],
        ..Default::default()
    };
    for engine in [Engine::MySql, Engine::Sqlite] {
        assert_eq!(parameter_columns(engine, &query).unwrap()[0].name, "pg_catalog_id");
    }
}

#[test]
fn a_row_comparison_refuses_a_parameter_that_sqlc_typed_by_another_column() {
    let wrong = || {
        param(
            2,
            of_authors(plugin::Column {
                original_name: "name".into(),
                ..column("before", "text", true)
            }),
        )
    };
    let run = |right: &str| {
        let query = plugin::Query {
            params: vec![param(1, column("name", "text", true)), wrong()],
            ..query(
                ":exec",
                &format!("DELETE FROM authors WHERE (name, id) < ($1, {right})"),
            )
        };
        check_parameters(Engine::PostgreSql, &query)
    };
    assert!(error_of(run("$2")).contains("sqlc typed it by \"name\""));
    run("$2::bigint").unwrap();
}

#[test]
fn a_postgres_parameter_needs_its_own_type_when_its_operand_differs() {
    for (comparison, ty) in [
        ("issued + $1", "date"),
        ("starts_at - $1", "timetz"),
        ("issued::date = $1", "timestamp"),
        ("CAST(issued AS date) = $1", "timestamp"),
        ("settings ? $1", "jsonb"),
        ("tags[$1]", "text"),
    ] {
        let column = of_authors(plugin::Column {
            is_array: comparison.starts_with("tags"),
            ..column("p", ty, true)
        });
        let check = |sql: &str| {
            check_parameters(
                Engine::PostgreSql,
                &plugin::Query {
                    params: vec![param(1, column.clone())],
                    ..query(":many", sql)
                },
            )
        };
        assert!(error_of(check(comparison)).contains("explicitly cast"), "{comparison}");
        check(&comparison.replace("$1", "$1::int")).unwrap();
    }
}

#[test]
fn row_comparisons_pair_each_uncast_placeholder_with_its_own_column() {
    for sql in [
        "SELECT * FROM t WHERE (t.at, t.\"ID\") < ($1, $2)",
        "SELECT * FROM t WHERE RoW($1, $2) >= rOw(t.at, t.\"ID\")",
    ] {
        assert_eq!(
            row_parameters(&Engine::PostgreSql.words(sql)),
            [(1, "at".into()), (2, "ID".into())]
        );
    }
    assert_eq!(
        row_parameters(&Engine::PostgreSql.words("SELECT * FROM t WHERE (at, id) < ($1, $2::uuid)")),
        [(1, "at".into())]
    );
    assert!(row_parameters(&Engine::PostgreSql.words("SELECT make_pair(at, id) = ($1, $2) FROM t")).is_empty());
}
