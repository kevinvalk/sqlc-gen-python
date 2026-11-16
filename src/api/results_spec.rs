use super::*;
use crate::api::fixtures::authors::*;
use crate::api::fixtures::*;
use crate::api::models::FieldType;
use crate::api::queries::{Command, Takes};
use crate::backend::Backend;
use crate::config::Config;
use crate::python::Type;

#[test]
fn the_commands_that_return_no_rows_ignore_any_columns() {
    // A `RETURNING` list sqlc reports is not theirs to read.
    let id = || vec![column("id", "int8", true)];
    assert_eq!(returning(":exec", id()), Command::Exec);
    assert_eq!(returning(":execrows", id()), Command::ExecRows);
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
        as_text: false,
    };
    assert_eq!(
        returning(":one", vec![column("id", "int8", true)]),
        Command::One(Shape::Scalar(id.clone()))
    );
    assert_eq!(
        returning(":many", vec![column("bio", "int8", false)]),
        Command::Many(Shape::Scalar(FieldType { optional: true, ..id }))
    );
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
        assert!(matches!(returning(":one", columns), Command::One(Shape::Row(..))));
    }
    assert!(matches!(returning(":one", unowned), Command::One(Shape::Row(..))));
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
        ..query(
            ":one",
            "SELECT authors.id, authors.name, authors.bio, t.total FROM authors JOIN t ON true",
        )
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
    let inner = "SELECT b.id, authors.id, authors.name, authors.bio FROM books b JOIN authors ON true";
    assert_eq!(embedded(inner), None);
    let left = "SELECT b.id, authors.id, authors.name, authors.bio FROM books b LEFT JOIN authors ON true";
    assert_eq!(embedded(left), Some(0));
    // Nor is a row of it proven by what is not read: a subquery under its
    // name, a join in parentheses.
    for unproven in [
        "SELECT b.id, authors.id, authors.name, authors.bio FROM authors RIGHT JOIN books b ON true",
        "SELECT b.id, authors.id, authors.name, authors.bio FROM books b \
         LEFT JOIN LATERAL (SELECT * FROM authors LIMIT 1) authors ON true",
        "SELECT b.id, authors.id, authors.name, authors.bio FROM books b \
         LEFT JOIN (authors JOIN publishers p ON true) ON true",
    ] {
        assert_eq!(embedded(unproven), Some(0), "{unproven}");
    }
}

#[test]
fn a_column_after_an_embed_is_read_at_its_place_in_the_statements_list() {
    // sqlc reports an embed as one column, and writes each of its table's.
    let embed = plugin::Column {
        name: "authors".into(),
        embed_table: Some(plugin::Identifier {
            name: "authors".into(),
            ..Default::default()
        }),
        ..Default::default()
    };
    let query = resolve(plugin::Query {
        columns: vec![embed, column("title", "text", true)],
        ..query(
            ":one",
            "SELECT authors.id, authors.name, authors.bio, \
             (SELECT b.title FROM books b LIMIT 1) AS title FROM authors",
        )
    });
    // The subquery may find no book, whatever sqlc reports.
    assert!(query.columns[3].ty.optional);
}

#[test]
fn a_list_that_is_not_read_leaves_every_column_optional() {
    // We do not know which item is which column, so trusting sqlc's NOT NULL of any would be a guess
    let query = resolve(plugin::Query {
        columns: vec![column("id", "int8", true), column("title", "text", true)],
        ..query(
            ":many",
            "SELECT a.id, (SELECT b.title FROM books b LIMIT 1), 1 AS unreported FROM authors a",
        )
    });
    let Command::Many(Shape::Row(_, fields)) = query.command else {
        panic!("not a row: {:?}", query.command);
    };
    let optional: Vec<bool> = (fields.iter())
        .map(|field| match field {
            RowField::Column(field) => field.ty.optional,
            RowField::Embed { .. } => panic!("an embed"),
        })
        .collect();
    assert_eq!(optional, [true, true]);
}

#[test]
fn an_ungrouped_aggregate_is_optional_though_sqlc_reports_its_cast_not_null() {
    let total = |sql: &str| {
        let query = resolve(plugin::Query {
            columns: vec![column("total", "pg_catalog.numeric", true)],
            ..query(":one", sql)
        });
        (query.command, query.raises)
    };
    let numeric = |optional| FieldType {
        element: Type::DECIMAL,
        array_dims: 0,
        optional,
        as_text: false,
    };
    // One row, whose value is NULL of no rows.
    assert_eq!(
        total("SELECT sum(amount)::numeric AS total FROM payments"),
        (Command::One(Shape::Scalar(numeric(true))), true)
    );
    assert_eq!(
        total("SELECT coalesce(sum(amount), 0)::numeric AS total FROM payments"),
        (Command::One(Shape::Scalar(numeric(false))), true)
    );
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
fn an_embed_of_a_model_without_fields_is_read_past_the_list() {
    // Every column of the table has a problem, so its model has no field, and the embed comes last, where the list has
    // no item left. The table's problem must be the error, not a panic!
    let pairs = table("pairs", vec![column("p", "pair", true)]);
    let embed = plugin::Column {
        name: "pairs".into(),
        embed_table: Some(plugin::Identifier {
            name: "pairs".into(),
            ..Default::default()
        }),
        ..Default::default()
    };
    let query = plugin::Query {
        columns: vec![column("id", "int8", true), embed],
        ..query(":many", "SELECT a.id FROM authors a, pairs")
    };
    let request = request(
        "postgresql",
        vec![sql_schema("public", vec![pairs], vec![])],
        vec![query],
    );
    let error = crate::api::resolve(&request, &Config::of(Backend::SqlAlchemy)).unwrap_err();
    let error = format!("{error:#}");
    assert!(
        error.contains(r#"table "pairs": column "p" has type "pair""#),
        "{error}"
    );
}

#[test]
fn a_mysql_cast_to_char_or_binary_says_why_it_has_no_type() {
    let error = on(
        "mysql",
        plugin::Query {
            columns: vec![column("label", "var_string", true)],
            ..query(":one", "SELECT CAST(id AS CHAR) AS label FROM authors")
        },
    )
    .unwrap_err();
    assert!(
        format!("{error:#}").ends_with(
            r#"column "label" has type "var_string", which has no verified Python mapping; name its Python type with an override (db_type "var_string"); it is how sqlc reports a CAST AS CHAR, a str, and a CAST AS BINARY, bytes, alike; or set unknown_type: Any to leave it unchecked"#
        ),
        "{error:#}"
    );
}

#[test]
fn arithmetic_on_mysql_and_sqlite_asks_for_a_cast() {
    // sqlc types `a + b` as `int`, MySQL's `1.0` as `float` and its `BINARY x` (bytes) as `char`, of no table and
    // whatever the operands, while it spells a cast as declared.
    for (engine, data_type) in [
        ("mysql", "int"),
        ("mysql", "float"),
        ("mysql", "char"),
        ("sqlite", "int"),
    ] {
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
    for (engine, reported) in [("postgresql", "bigint"), ("mysql", "bigint"), ("sqlite", "integer")] {
        let call = |name: &str| plugin::Column {
            is_func_call: true,
            ..column(name, reported, true)
        };
        let counted = on(
            engine,
            plugin::Query {
                columns: vec![call("count"), call("n")],
                ..query(":one", "SELECT COUNT(*), count(DISTINCT bio) AS n FROM authors")
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
        assert!(format!("{summed:#}").contains("is a function's result"), "{engine}");
    }
}

#[test]
fn an_embed_that_may_be_missing_needs_a_not_null_column_to_tell_by() {
    // A table of nullable columns alone: a row of NULLs and no row read alike.
    let notes = table("notes", vec![column("body", "text", false)]);
    let embed = plugin::Column {
        name: "notes".into(),
        embed_table: Some(plugin::Identifier {
            name: "notes".into(),
            ..Default::default()
        }),
        ..Default::default()
    };
    let query = plugin::Query {
        columns: vec![column("id", "int8", true), embed],
        ..query(
            ":many",
            "SELECT a.id, notes.body FROM authors a LEFT JOIN notes ON true",
        )
    };
    let request = request(
        "postgresql",
        vec![sql_schema("public", vec![notes], vec![])],
        vec![query],
    );
    let error = crate::api::resolve(&request, &Config::of(Backend::SqlAlchemy)).unwrap_err();
    let error = format!("{error:#}");
    assert!(error.contains("none is NOT NULL to tell a missing row by"), "{error}");
}
