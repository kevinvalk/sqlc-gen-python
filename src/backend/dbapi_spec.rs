//! Specification for running sqlc's SQL as written, on a driver.

use super::*;
use crate::api::models::{Field, FieldType, Member};
use crate::api::queries::Command;
use crate::backend::Backend;
use crate::engine::{Piece, Sql};
use crate::python::{self, Type};

fn field(name: &str, element: Type) -> Field {
    Field {
        name: name.into(),
        comment: String::new(),
        ty: FieldType {
            element,
            array_dims: 0,
            optional: false,
        },
    }
}

/// A query `get` taking `params`, on `$1`, `$2`, ...
fn query(command: Command, params: Vec<Field>) -> Query {
    let mut pieces = vec![Piece::Text("SELECT ".into())];
    for (i, param) in params.iter().enumerate() {
        pieces.push(Piece::Param {
            placeholder: format!("${}", i + 1),
            name: param.name.clone(),
        });
    }
    Query {
        function: "get".into(),
        constant: "_GET".into(),
        comment: String::new(),
        sql: Sql(pieces),
        copy_into: None,
        rows: None,
        params,
        columns: Vec::new(),
        command,
    }
}

#[test]
fn a_driver_is_given_sqlcs_statement_as_written() {
    let query = query(Command::Exec, vec![field("id", Type::INT)]);
    let statement = python::render(
        &[python::constant("_GET", Backend::Psycopg.statement(&query))],
        &[],
    );
    assert!(
        statement.ends_with("_GET: Final = \"SELECT $1\"\n"),
        "{statement}"
    );
}

#[test]
fn a_driver_binds_each_parameter_by_position_json_wrapped_on_psycopg() {
    let params = vec![field("id", Type::INT), field("data", Type::JSONB)];
    let (_, body) = Backend::Psycopg.body(&query(Command::Exec, params.clone()), false);
    let body = python::stmts_source(&body);
    assert_eq!(
        body,
        "with psycopg.RawCursor(conn, row_factory=tuple_row) as cur:\n    \
         cur.execute(_GET, (id, None if data is None else Jsonb(data)))"
    );
    let (_, body) = Backend::Sqlite3.body(&query(Command::Exec, params[..1].to_vec()), true);
    let body = python::stmts_source(&body);
    assert_eq!(
        body,
        "async with conn.cursor() as cur:\n    \
         cur.row_factory = None\n    \
         await cur.execute(_GET, (id,))"
    );
}

#[test]
fn a_one_raises_without_exactly_one_row_on_a_driver() {
    let one = Command::One(crate::api::queries::Shape::Scalar(field("n", Type::INT).ty));
    let (returns, body) = Backend::Sqlite3.body(&query(one, Vec::new()), false);
    let body = python::stmts_source(&body);
    assert_eq!(python::python_source(&returns), "int");
    assert_eq!(
        body,
        "with contextlib.closing(conn.cursor()) as cur:\n    \
         cur.row_factory = None\n    \
         cur.execute(_GET)\n    \
         match cur.fetchmany(2):\n        \
         case [row]:\n            value: int = row[0]\n            return value\n        \
         case []:\n            raise NoRowError\n        \
         case _:\n            raise MultipleRowsError"
    );
}

#[test]
fn psycopg_reads_an_array_of_an_enum_only_if_no_label_is_quoted_in_it() {
    let sql_enum = |labels: &[&str]| Enum {
        name: "Method".into(),
        comment: String::new(),
        members: (labels.iter())
            .map(|label| Member {
                name: label.to_uppercase(),
                value: (*label).into(),
            })
            .collect(),
    };
    assert!(check_text_array(&sql_enum(&["card", "direct-debit", "a_b"])).is_ok());
    for label in [
        "",
        "null",
        "NULL",
        "with space",
        "a,b",
        "{a}",
        "\"q\"",
        "back\\slash",
    ] {
        let error = check_text_array(&sql_enum(&["card", label])).unwrap_err();
        assert!(
            error.to_string().contains("quotes the label"),
            "{label:?}: {error}"
        );
    }
    // Neither SQLAlchemy nor a driver that returns lists needs one.
    assert!(
        Backend::SqlAlchemy
            .check_array_enum(&sql_enum(&["with space"]))
            .is_ok()
    );
}

#[test]
fn pymysql_takes_percent_s_and_every_other_percent_doubled() {
    let mut query = query(Command::Exec, vec![field("p", Type::STR)]);
    query.sql.0[1] = Piece::Param {
        placeholder: "?".into(),
        name: "p".into(),
    };
    query.sql.0.push(Piece::Text(" LIKE '%:x%'".into()));
    let statement = python::render(
        &[python::constant("_GET", Backend::Pymysql.statement(&query))],
        &[],
    );
    assert!(
        statement.contains(r#""SELECT %s LIKE '%%:x%%'""#),
        "{statement}"
    );
    // Without parameters PyMySQL formats nothing, so nothing is doubled.
    let query = Query {
        sql: Sql(vec![Piece::Text("SELECT '%:x%'".into())]),
        params: Vec::new(),
        ..query
    };
    let statement = python::render(
        &[python::constant("_GET", Backend::Pymysql.statement(&query))],
        &[],
    );
    assert!(statement.contains(r#""SELECT '%:x%'""#), "{statement}");
}

#[test]
fn a_slice_is_one_placeholder_per_item_and_a_subquery_of_none_when_empty() {
    let mut query = query(Command::Exec, vec![field("ids", Type::INT)]);
    query.sql.0[1] = Piece::Slice {
        marker: "/*SLICE:ids*/".into(),
        placeholder: "?".into(),
        name: "ids".into(),
    };
    let (_, body) = Backend::Pymysql.body(&query, false);
    let body = python::stmts_source(&body);
    assert!(
        body.contains(
            r#"cur.execute(_GET.replace("/*SLICE:ids*/%s", ", ".join(["%s"] * len(ids)) or "SELECT NULL FROM DUAL WHERE FALSE"), (*ids,))"#
        ),
        "{body}"
    );
}
