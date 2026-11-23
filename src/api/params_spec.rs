use super::*;
use crate::api::fixtures::authors::{params, *};
use crate::api::fixtures::*;
use crate::backend::Backend;
use crate::config::Config;
use crate::engine::Piece;
use crate::python::Type;

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
    let placeholders: Vec<_> = (query.sql.0.iter())
        .filter_map(|piece| match piece {
            Piece::Param { name, argument, .. } => Some((name.as_str(), *argument)),
            _ => None,
        })
        .collect();
    assert_eq!(placeholders, [("from_", 0), ("column_2", 1), ("from_", 0)]);
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
    // `interval`, so `x: int` would push a caller to drop an amount's cents.
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
fn a_mysql_name_may_start_with_a_dollar_sign() {
    // `sqlc.arg($x)` names a parameter as a column `$x` would be named, but PostgreSQL reads `$x` as no identifier.
    let resolved = on(
        "mysql",
        plugin::Query {
            params: vec![named(1, "$x", "int", true)],
            ..query(":exec", "SELECT 1 WHERE a = ?")
        },
    )
    .unwrap();
    assert_eq!(resolved.params[0].name, "x");

    let error = error(plugin::Query {
        params: vec![named(1, "$x", "int4", true)],
        ..query(":exec", "SELECT $1")
    });
    assert!(
        error.contains(r#"sqlc named a parameter "$x", which is no name"#),
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
        argument: 0,
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

#[test]
fn a_use_sqlc_could_not_type_takes_the_type_of_the_use_it_typed() {
    // `sqlc.narg(q) IS NULL OR b = sqlc.narg(q)`: MySQL reports the use that only meets NULL as a nullable `any`, in
    // either order, and the other as nullable for a `sqlc.narg`.
    for uses in [
        vec![named(1, "q", "any", false), named(2, "q", "text", false)],
        vec![named(1, "q", "text", false), named(2, "q", "any", false)],
    ] {
        let resolved = on(
            "mysql",
            plugin::Query {
                params: uses,
                ..query(":exec", "SELECT 1 WHERE (? IS NULL OR b = ?)")
            },
        )
        .unwrap();
        assert_eq!(resolved.params.len(), 1);
        assert_eq!(
            resolved.params[0].ty.describe(crate::python::Scope::Queries),
            "str | None"
        );
    }
    // sqlc reports a use it could not type as admitting NULL, even of a `sqlc.arg` (`sqlc.arg(id) = id OR
    // sqlc.arg(id) = 0`), and we never second-guess what it reports.
    let resolved = on(
        "mysql",
        plugin::Query {
            params: vec![named(1, "id", "int", true), named(2, "id", "any", false)],
            ..query(":exec", "SELECT 1 WHERE (? = id OR ? = 0)")
        },
    )
    .unwrap();
    assert_eq!(
        resolved.params[0].ty.describe(crate::python::Scope::Queries),
        "int | None"
    );
    // With no use sqlc typed there is nothing to take.
    let error = on(
        "mysql",
        plugin::Query {
            params: vec![named(1, "q", "any", false), named(2, "q", "any", false)],
            ..query(":exec", "SELECT 1 WHERE (? IS NULL OR ? IS NULL)")
        },
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("could not infer"), "{error:#}");
}

#[test]
fn uses_of_one_type_are_one_parameter_whether_an_override_or_the_type_map_names_it() {
    // We keep the first use's type, which binds a `str` as the other would.
    let config = Config {
        overrides: serde_json::from_str(r#"[{"db_type": "var_string", "python_type": "str"}]"#).unwrap(),
        ..Config::of(Backend::Pymysql)
    };
    let modules = try_resolve_with(
        "mysql",
        vec![plugin::Query {
            params: vec![named(1, "q", "var_string", false), named(2, "q", "text", false)],
            ..query(":exec", "SELECT 1 WHERE (CAST(? AS CHAR) IS NULL OR b = ?)")
        }],
        &config,
    )
    .unwrap();
    let params = &modules[0].queries[0].params;
    assert_eq!(params.len(), 1);
    assert_eq!(params[0].ty.describe(crate::python::Scope::Queries), "str | None");
}
