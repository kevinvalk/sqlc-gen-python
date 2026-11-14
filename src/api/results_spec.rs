//! Specification for what a query's rows become.

use super::*;
use crate::api::fixtures::authors::*;
use crate::api::fixtures::*;
use crate::api::models::FieldType;
use crate::api::queries::{Command, Takes};
use crate::python::Type;

#[test]
fn the_commands_that_return_no_rows_ignore_any_columns() {
    assert_eq!(returning(":exec", vec![]), Command::Exec);
    assert_eq!(returning(":execrows", vec![]), Command::ExecRows);
    assert_eq!(returning(":execresult", vec![]), Command::ExecResult);
    // A `:copyfrom` is an `:exec` of each row.
    let copy = resolve(self::query(":copyfrom", "SELECT 1"));
    assert_eq!(copy.command, Command::Exec);
    assert_eq!(copy.takes, Takes::Rows("GetAuthorParams".into()));
}

#[test]
fn a_single_column_is_returned_bare() {
    let id = FieldType {
        element: Type::INT,
        array_dims: 0,
        optional: false,
    };
    assert_eq!(
        returning(":one", vec![column("id", "int8", true)]),
        Command::One(Shape::Scalar(id.clone()))
    );
    assert_eq!(
        returning(":many", vec![column("bio", "int8", false)]),
        Command::Many(Shape::Scalar(FieldType {
            optional: true,
            ..id
        }))
    );
}

#[test]
fn a_nullable_or_json_single_column_is_returned_bare_since_no_row_raises() {
    // `None` can only mean NULL (or JSON `null`): a missing row raises.
    for (name, ty, not_null) in [("bio", "text", false), ("body", "jsonb", true)] {
        for cmd in [":one", ":many"] {
            let command = returning(cmd, vec![column(name, ty, not_null)]);
            assert!(
                matches!(
                    command,
                    Command::One(Shape::Scalar(_)) | Command::Many(Shape::Scalar(_))
                ),
                "{cmd} {ty}: {command:?}"
            );
        }
    }
}

#[test]
fn a_result_of_exactly_a_tables_columns_reuses_its_model() {
    let columns = authors().into_iter().map(of_authors).collect();
    assert_eq!(
        returning(":many", columns),
        Command::Many(Shape::Model("Author".into()))
    );
}

#[test]
fn a_result_that_differs_from_the_table_in_any_way_gets_its_own_row() {
    let mut nullable = authors();
    nullable[0].not_null = false;
    let mut renamed = authors();
    renamed[2].name = "biography".into();
    let mut unowned = authors().into_iter().map(of_authors).collect::<Vec<_>>();
    unowned[1].table = None;
    for columns in [
        authors()[..2].to_vec(),
        nullable,
        renamed,
        authors().into_iter().rev().collect(),
    ] {
        let columns: Vec<_> = columns.into_iter().map(of_authors).collect();
        assert!(matches!(
            returning(":one", columns),
            Command::One(Shape::Row(..))
        ));
    }
    assert!(matches!(
        returning(":one", unowned),
        Command::One(Shape::Row(..))
    ));
}

#[test]
fn repeated_result_columns_are_numbered_and_unnamed_ones_named_by_position() {
    let Command::Many(Shape::Row(_, fields)) = returning(
        ":many",
        vec![
            column("id", "int8", true),
            column("id", "int8", true),
            column("", "text", true),
        ],
    ) else {
        panic!("not a row");
    };
    let names: Vec<&str> = (fields.iter())
        .map(|field| match field {
            RowField::Column(field) => field.name.as_str(),
            RowField::Embed { name, .. } => name.as_str(),
        })
        .collect();
    assert_eq!(names, ["id", "id_2", "column_3"]);
}

#[test]
fn an_embed_is_a_field_of_its_model_over_the_tables_columns() {
    let embed = plugin::Column {
        name: "authors".into(),
        embed_table: Some(plugin::Identifier {
            name: "authors".into(),
            ..Default::default()
        }),
        ..Default::default()
    };
    let query = resolve(plugin::Query {
        columns: vec![embed, column("total", "int8", true)],
        ..query(":one", "SELECT 1")
    });
    let Command::One(Shape::Row(_, fields)) = &query.command else {
        panic!("not a row");
    };
    assert_eq!(
        fields[0],
        RowField::Embed {
            name: "author".into(),
            model: "Author".into(),
            width: 3,
            missing: None,
        }
    );
    let flat: Vec<&str> = query.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(flat, ["id", "name", "bio", "total"]);
}

#[test]
fn an_embed_an_outer_join_may_leave_without_a_row_is_none_then() {
    let embed = plugin::Column {
        name: "authors".into(),
        embed_table: Some(plugin::Identifier {
            name: "authors".into(),
            ..Default::default()
        }),
        ..Default::default()
    };
    let embedded = |sql: &str| {
        let query = resolve(plugin::Query {
            columns: vec![column("id", "int8", true), embed.clone()],
            ..query(":one", sql)
        });
        let Command::One(Shape::Row(_, fields)) = query.command else {
            panic!("not a row");
        };
        match &fields[1] {
            RowField::Embed { missing, .. } => *missing,
            RowField::Column(_) => panic!("not an embed"),
        }
    };
    // `authors.id` is NOT NULL: NULL only where the join found no author.
    let inner =
        "SELECT b.id, authors.id, authors.name, authors.bio FROM books b JOIN authors ON true";
    assert_eq!(embedded(inner), None);
    let left =
        "SELECT b.id, authors.id, authors.name, authors.bio FROM books b LEFT JOIN authors ON true";
    assert_eq!(embedded(left), Some(0));
    let right = "SELECT b.id, authors.id, authors.name, authors.bio FROM authors RIGHT JOIN books b ON true";
    assert_eq!(embedded(right), Some(0));
}

#[test]
fn an_embed_of_a_table_the_catalog_lacks_is_an_error() {
    let embed = plugin::Column {
        name: "ghosts".into(),
        embed_table: Some(plugin::Identifier {
            name: "ghosts".into(),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(
        error(plugin::Query {
            columns: vec![embed],
            ..query(":one", "SELECT 1")
        })
        .ends_with("sqlc.embed(ghosts) names no table")
    );
}

#[test]
fn arithmetic_on_mysql_and_sqlite_asks_for_a_cast() {
    // sqlc types `a + b` as `int`, and MySQL's `1.0` as `float`, of no table,
    // whatever the operands; a cast it spells as declared.
    for (engine, data_type) in [("mysql", "int"), ("mysql", "float"), ("sqlite", "int")] {
        let error = on(
            engine,
            plugin::Query {
                columns: vec![column("total", data_type, true)],
                ..query(":one", "SELECT a + b AS total FROM t")
            },
        )
        .unwrap_err();
        assert!(
            format!("{error:#}").contains("typed by its form"),
            "{engine} {data_type}: {error:#}"
        );
    }
    let cast = on(
        "sqlite",
        plugin::Query {
            columns: vec![column("total", "INTEGER", true)],
            ..query(":one", "SELECT CAST(a + b AS INTEGER) AS total FROM t")
        },
    );
    assert!(cast.is_ok());
}

#[test]
fn a_count_is_an_int_on_every_engine_and_any_other_call_asks_for_a_cast() {
    for (engine, reported) in [
        ("postgresql", "bigint"),
        ("mysql", "bigint"),
        ("sqlite", "integer"),
    ] {
        let call = |name: &str| plugin::Column {
            is_func_call: true,
            ..column(name, reported, true)
        };
        let counted = on(
            engine,
            plugin::Query {
                columns: vec![call("count"), call("n")],
                ..query(
                    ":one",
                    "SELECT COUNT(*), count(DISTINCT bio) AS n FROM authors",
                )
            },
        )
        .unwrap();
        let Command::One(Shape::Row(_, fields)) = &counted.command else {
            panic!("not a row: {:?}", counted.command);
        };
        let annotations: Vec<String> = (fields.iter())
            .map(|field| match field {
                RowField::Column(field) => field.ty.describe(crate::python::Scope::Queries),
                RowField::Embed { .. } => "embed".into(),
            })
            .collect();
        assert_eq!(annotations, ["int", "int"], "{engine}");
        let summed = on(
            engine,
            plugin::Query {
                columns: vec![call("sum")],
                ..query(":one", "SELECT sum(id) FROM authors")
            },
        )
        .unwrap_err();
        assert!(
            format!("{summed:#}").contains("is a function's result"),
            "{engine}"
        );
    }
}
