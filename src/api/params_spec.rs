use super::*;
use crate::api::fixtures::authors::{params, *};
use crate::api::fixtures::*;
use crate::backend::Backend;
use crate::config::Config;
use crate::engine::Piece;

#[test]
fn placeholders_become_the_parameters_python_names() {
    let query = params(
        vec![
            param(1, column("from", "text", true)),
            param(2, column("", "int4", true)),
        ],
        "SELECT $1, $2::int, $1",
    );
    let names: Vec<&str> = query.params.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["from_", "column_2"]);
    let placeholders: Vec<&str> = (query.sql.0.iter())
        .filter_map(|piece| match piece {
            Piece::Param { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(placeholders, ["from_", "column_2", "from_"]);
    assert_eq!(query.sql.text(), "SELECT $1, $2::int, $1");
}

#[test]
fn a_parameter_name_the_user_chose_is_never_numbered() {
    // `"userId" = $1 AND uid = sqlc.arg('user_id')`: numbering would bind the
    // user's `user_id` to `$1`, with the same type, so a swap type-checks.
    let named = plugin::Column {
        is_named_param: true,
        ..column("user_id", "int4", false)
    };
    let error = error(plugin::Query {
        params: vec![param(1, column("userId", "int4", false)), param(2, named)],
        ..query(":exec", "SELECT $1, $2")
    });
    assert!(
        error.contains(r#"parameters "userId" and "user_id" both become the Python name "user_id""#),
        "{error}"
    );
}

#[test]
fn a_parameter_sqlc_named_after_an_expression_is_an_error() {
    // `@day::date AT TIME ZONE 'Europe/Amsterdam'`: sqlc replaced the whole
    // expression with `$1`, dropping the time zone, and named it so. With
    // 'UTC' the name is an identifier, but still a flattened function call;
    // `@x + 1` is named "+x". And `sqlc.arg('my id')` is named as written.
    for name in [
        "pg_catalogtimezoneEurope/Amsterdamdaydate",
        "pg_catalogtimezoneUTCdaytimestamptz",
        "+x",
        "my id",
    ] {
        let named = plugin::Column {
            is_named_param: true,
            ..column(name, "timestamptz", true)
        };
        let error = error(plugin::Query {
            params: vec![param(1, named)],
            ..query(":exec", "SELECT $1")
        });
        assert!(
            error.contains(&format!(r#"sqlc named a parameter "{name}", which is no name"#)),
            "{error}"
        );
    }
    // What a column gave its name to is the column's, whatever it is.
    let query = params(vec![param(1, column("my col", "text", true))], "SELECT $1");
    assert_eq!(query.params[0].name, "my_col");
}

#[test]
fn a_repeated_parameter_name_is_numbered_as_sqlc_numbers_it() {
    let query = params(
        vec![
            param(1, column("issued_on", "date", false)),
            param(2, column("issued_on", "date", false)),
            param(3, column("issued_on", "date", false)),
        ],
        "SELECT $1, $2, $3",
    );
    let names: Vec<&str> = query.params.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["issued_on", "issued_on_2", "issued_on_3"]);
}

#[test]
fn a_parameter_is_optional_exactly_when_sqlc_says_it_admits_null() {
    let query = params(
        vec![
            param(1, column("a", "text", true)),
            param(2, column("b", "text", false)),
        ],
        "SELECT $1, $2",
    );
    let optional: Vec<bool> = query.params.iter().map(|p| p.ty.optional).collect();
    assert_eq!(optional, [false, true]);
}

#[test]
fn a_json_parameter_keeps_which_json_type_it_is() {
    let query = params(
        vec![
            param(1, column("a", "json", false)),
            param(2, column("b", "pg_catalog.jsonb", false)),
            param(3, column("c", "text", false)),
        ],
        "SELECT $1, $2, $3",
    );
    let types: Vec<&Type> = query.params.iter().map(|p| &p.ty.element).collect();
    assert_eq!(types, [&Type::JSON, &Type::JSONB, &Type::STR]);
}

#[test]
fn an_unprobed_parameter_is_an_error_naming_its_number_and_name() {
    let mut array = column("docs", "jsonb", true);
    (array.is_array, array.array_dims) = (true, 1);
    let unknown = |column| {
        error(plugin::Query {
            params: vec![param(2, column)],
            ..query(":exec", "SELECT $2")
        })
    };
    assert!(
        unknown(column("p", "point", true))
            .contains(r#"parameter $2 ("p") has type "point", which has no verified Python mapping"#)
    );
    assert!(unknown(array).contains(r#"parameter $2 ("docs") is an array of "jsonb""#));
    // A parameter's cast is written on the placeholder, not coalesced.
    let uninferred = unknown(column("x", "", false));
    assert!(
        uninferred.contains(
            r#"parameter $2 ("x") has a type sqlc could not infer; add a cast, as @x::<type>, or sqlc.narg('x')::<type> if it can be NULL"#
        ),
        "{uninferred}"
    );
}

#[test]
fn a_parameter_typed_by_a_guessed_overload_asks_for_a_cast() {
    // sqlc types `abs(@x)` by the first overload of `abs` it lists, `bigint`, and `date_trunc('month', @d)` by
    // `interval`, so `x: int` would push a caller to drop an amount's cents. LIMIT and OFFSET are sqlc's own `integer`,
    // which is right.
    for (name, data_type) in [("x", "bigint"), ("d", "interval")] {
        let error = error(plugin::Query {
            params: vec![param(1, column(name, data_type, true))],
            ..query(":exec", "SELECT $1")
        });
        assert!(
            error.contains(&format!(
                r#"parameter $1 ("{name}") has a type sqlc could not infer; add a cast, as @{name}::<type>"#
            )),
            "{error}"
        );
    }
    let limit = params(vec![param(1, column("lim", "integer", true))], "SELECT 1 LIMIT $1");
    assert_eq!(limit.params[0].ty.element, Type::INT);
}

#[test]
fn sqlc_slice_is_an_error_that_says_what_postgresql_uses_instead() {
    let mut ids = column("ids", "int4", true);
    ids.is_sqlc_slice = true;
    let error = error(plugin::Query {
        params: vec![param(1, ids)],
        ..query(":exec", "SELECT $1")
    });
    assert!(
        error.contains("sqlc.slice is MySQL/SQLite only; on PostgreSQL use = ANY(@ids::type[])"),
        "{error}"
    );
}

#[test]
fn a_name_the_user_chose_is_one_parameter_wherever_mysql_binds_it() {
    // MySQL's `?` cannot be bound twice, so sqlc reports each use.
    let resolved = on(
        "mysql",
        plugin::Query {
            params: vec![named(1, "q", "text", true), named(2, "q", "text", false)],
            ..query(":exec", "SELECT 1 WHERE a = ? OR b = ?")
        },
    )
    .unwrap();
    let names: Vec<&str> = resolved.params.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["q"]);
    let bound: Vec<&str> = (resolved.sql.0.iter())
        .filter_map(|piece| match piece {
            Piece::Param { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(bound, ["q", "q"]);
    // Either use admitting NULL makes it optional.
    assert!(resolved.params[0].ty.optional);
    // Two uses of two types cannot be one parameter.
    let error = on(
        "mysql",
        plugin::Query {
            params: vec![named(1, "q", "text", true), named(2, "q", "int", true)],
            ..query(":exec", "SELECT 1 WHERE a = ? OR b = ?")
        },
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("is used as str and as int"), "{error:#}");
}

#[test]
fn a_slice_is_a_list_of_its_type_bound_one_placeholder_per_item() {
    let slice = param(
        1,
        plugin::Column {
            is_sqlc_slice: true,
            ..column("ids", "integer", true)
        },
    );
    let resolved = on(
        "sqlite",
        plugin::Query {
            params: vec![slice.clone()],
            ..query(":exec", "SELECT 1 WHERE id IN (/*SLICE:ids*/?)")
        },
    )
    .unwrap();
    assert!(resolved.sql.is_slice("ids"));
    assert_eq!(
        resolved.params[0].ty.describe(crate::python::Scope::Queries),
        "list[int]"
    );
    assert!(resolved.sql.0.contains(&Piece::Slice {
        marker: "/*SLICE:ids*/".into(),
        placeholder: "?".into(),
        number: 1,
        name: "ids".into(),
    }));
    let error = on(
        "postgresql",
        plugin::Query {
            params: vec![slice],
            ..query(":exec", "SELECT 1 WHERE id IN ($1)")
        },
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("sqlc.slice is MySQL/SQLite only"));
}

#[test]
fn a_rename_that_makes_two_chosen_parameters_one_name_is_an_error() {
    let mut config = Config::of(Backend::SqlAlchemy);
    config.rename = [("b".to_string(), "a".to_string())].into();
    let error = try_resolve_with(
        "mysql",
        vec![plugin::Query {
            params: vec![named(1, "a", "text", true), named(2, "b", "text", true)],
            ..query(":exec", "SELECT 1 WHERE x = ? AND y = ?")
        }],
        &config,
    )
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("both become the Python name \"a\""),
        "{error:#}"
    );
}

#[test]
fn a_slice_of_a_nullable_column_is_a_list_never_none() {
    let slice = param(
        1,
        plugin::Column {
            is_sqlc_slice: true,
            ..column("ids", "integer", false)
        },
    );
    let resolved = on(
        "sqlite",
        plugin::Query {
            params: vec![slice],
            ..query(":exec", "SELECT 1 WHERE id IN (/*SLICE:ids*/?)")
        },
    )
    .unwrap();
    let ty = &resolved.params[0].ty;
    assert_eq!(ty.describe(crate::python::Scope::Queries), "list[int]");
}
