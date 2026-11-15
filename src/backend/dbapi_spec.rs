use super::*;
use crate::api::models::{Field, FieldType, Member};
use crate::api::queries::{Command, Takes};
use crate::backend::Backend;
use crate::engine::{Piece, Sql};
use crate::python::vocabulary::FINAL;
use crate::python::{self, Type};

fn field(name: &str, element: Type) -> Field {
    Field {
        name: name.into(),
        comment: String::new(),
        ty: FieldType {
            element,
            array_dims: 0,
            optional: false,
            as_text: false,
        },
    }
}

/// A query `get` with its parameters on `$1`, `$2`, ...
fn query(command: Command, parameters: Vec<Field>) -> Query {
    let mut pieces = vec![Piece::Text("SELECT ".into())];
    for (i, parameter) in parameters.iter().enumerate() {
        pieces.push(Piece::Param {
            placeholder: format!("${}", i + 1),
            number: i as i32 + 1,
            name: parameter.name.clone(),
        });
    }
    Query {
        function: "get".into(),
        constant: "_GET".into(),
        comment: String::new(),
        sql: Sql(pieces),
        copy_into: None,
        raises: true,
        takes: Takes::Keywords,
        params: parameters,
        columns: Vec::new(),
        command,
    }
}

#[test]
fn a_driver_is_given_sqlcs_statement_as_written() {
    let query = query(Command::Exec, vec![field("id", Type::INT)]);
    let statement = python::render(
        &[python::ann_assign("_GET", FINAL, Backend::Psycopg.statement(&query))],
        &[],
    );
    assert!(statement.ends_with("_GET: Final = \"SELECT $1\"\n"), "{statement}");
}

#[test]
fn a_driver_binds_each_parameter_by_position_json_wrapped_on_psycopg() {
    let parameters = vec![field("id", Type::INT), field("data", Type::JSONB)];
    let body = Backend::Psycopg.body(&query(Command::Exec, parameters.clone()), false);
    let body = python::unparse_stmts(&body);
    assert_eq!(
        body,
        "with psycopg.RawCursor(connection, row_factory=tuple_row) as cur:\n    \
         cur.execute(_GET, (id, None if data is None else Jsonb(data)))"
    );
    let body = Backend::Sqlite3.body(&query(Command::Exec, parameters[..1].to_vec()), true);
    let body = python::unparse_stmts(&body);
    assert_eq!(
        body,
        "async with connection.cursor() as cur:\n    \
         await cur.execute(_GET, (id,))"
    );
}

#[test]
fn sqlite3_makes_a_cursors_rows_tuples_only_where_they_are_read() {
    let body = |command| python::unparse_stmts(&Backend::Sqlite3.body(&query(command, Vec::new()), false));
    let scalar = crate::api::queries::Shape::Scalar(field("n", Type::INT).ty);
    for reads in [
        Command::One(scalar.clone()),
        Command::Many(scalar),
        // Read by whoever the cursor is returned to
        Command::ExecResult,
    ] {
        let body = body(reads);
        assert!(body.contains("cur.row_factory = None\n"), "{body}");
    }
    for runs in [Command::Exec, Command::ExecRows] {
        let body = body(runs);
        assert!(!body.contains("row_factory"), "{body}");
    }
}

#[test]
fn a_one_raises_without_exactly_one_row_on_a_driver_if_asked() {
    let one = Command::One(crate::api::queries::Shape::Scalar(field("n", Type::INT).ty));
    let query = query(one, Vec::new());
    let returns = Backend::Sqlite3.returns(&query, false);
    let body = python::unparse_stmts(&Backend::Sqlite3.body(&query, false));
    assert_eq!(python::unparse(&returns), "int");
    assert_eq!(
        body,
        "with contextlib.closing(connection.cursor()) as cur:\n    \
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
    for label in ["", "null", "NULL", "with space", "a,b", "{a}", "\"q\"", "back\\slash"] {
        let error = check_text_array(&sql_enum(&["card", label])).unwrap_err();
        assert!(error.to_string().contains("quotes the label"), "{label:?}: {error}");
    }
    // Neither SQLAlchemy nor a driver that returns lists needs the check
    assert!(Backend::SqlAlchemy.check_array_enum(&sql_enum(&["with space"])).is_ok());
}

#[test]
fn pymysql_takes_percent_s_and_every_other_percent_doubled() {
    let mut query = query(Command::Exec, vec![field("p", Type::STR)]);
    query.sql.0[1] = Piece::Param {
        placeholder: "?".into(),
        number: 1,
        name: "p".into(),
    };
    query.sql.0.push(Piece::Text(" LIKE '%:x%'".into()));
    let statement = python::render(
        &[python::ann_assign("_GET", FINAL, Backend::Pymysql.statement(&query))],
        &[],
    );
    assert!(statement.contains(r#""SELECT %s LIKE '%%:x%%'""#), "{statement}");
    // Without parameters PyMySQL formats nothing, so nothing is doubled
    let query = Query {
        sql: Sql(vec![Piece::Text("SELECT '%:x%'".into())]),
        params: Vec::new(),
        ..query
    };
    let statement = python::render(
        &[python::ann_assign("_GET", FINAL, Backend::Pymysql.statement(&query))],
        &[],
    );
    assert!(statement.contains(r#""SELECT '%:x%'""#), "{statement}");
}

#[test]
fn only_a_library_whose_stubs_need_it_has_a_prelude() {
    for (backend, asynchronous) in [
        (Backend::SqlAlchemy, false),
        (Backend::SqlAlchemy, true),
        (Backend::Psycopg, false),
        (Backend::Psycopg, true),
        (Backend::Sqlite3, false),
        (Backend::Sqlite3, true),
    ] {
        assert!(backend.prelude(asynchronous).is_empty(), "{backend:?}");
    }
}

#[test]
fn pymysqls_connection_is_named_once_for_a_type_checker_and_for_python() {
    // Bare, a type checker takes the class for a connection of plain cursors only, and subscripted it raises, as it is
    // only generic in its stubs.
    let prelude = python::render(&Backend::Pymysql.prelude(false), &[]);
    assert_eq!(
        prelude,
        r"import typing
from typing import Any

import pymysql

if typing.TYPE_CHECKING:
    _Connection = pymysql.Connection[Any]
else:
    _Connection = pymysql.Connection
"
    );
    let annotation = python::unparse(&Backend::Pymysql.connection(false));
    assert_eq!(annotation, "_Connection");
}

#[test]
fn asyncmys_untyped_methods_are_called_through_a_protocol_of_the_modules_own() {
    let prelude = python::render(&Backend::Pymysql.prelude(true), &[]);
    assert_eq!(
        prelude,
        r"import typing
from collections.abc import Sequence
from typing import Any

import asyncmy.cursors


class _Cursor(typing.Protocol):
    async def execute(self, query: str, args: Sequence[Any] | None = None) -> int: ...

    async def executemany(
        self, query: str, args: Sequence[Sequence[Any]]
    ) -> object: ...

    async def fetchmany(self, size: int) -> Sequence[Sequence[Any]]: ...


def _typed(cursor: asyncmy.cursors.Cursor) -> _Cursor:
    return cursor
"
    );

    let scalar = crate::api::queries::Shape::Scalar(field("n", Type::INT).ty);
    let one = query(Command::One(scalar.clone()), Vec::new());
    let body = python::unparse_stmts(&Backend::Pymysql.body(&one, true));
    assert!(
        body.starts_with(
            "async with connection.cursor(asyncmy.cursors.Cursor) as cur:\n    \
             await _typed(cur).execute(_GET)\n    \
             match (await _typed(cur).fetchmany(2)):\n"
        ),
        "{body}"
    );
    // The stubs do type the rest: what reads every row, and the last id
    let many = query(Command::Many(scalar), Vec::new());
    let body = python::unparse_stmts(&Backend::Pymysql.body(&many, true));
    assert!(body.contains("in await cur.fetchall()]"), "{body}");
    let last_id = query(Command::ExecLastId, Vec::new());
    let body = python::unparse_stmts(&Backend::Pymysql.body(&last_id, true));
    assert!(body.ends_with("return cur.lastrowid"), "{body}");
    // An `:execresult` returns the library's cursor and not the protocol
    let result = query(Command::ExecResult, Vec::new());
    let returns = python::unparse(&Backend::Pymysql.returns(&result, true));
    let body = python::unparse_stmts(&Backend::Pymysql.body(&result, true));
    assert_eq!(returns, "asyncmy.cursors.Cursor");
    assert_eq!(
        body,
        "cur = connection.cursor(asyncmy.cursors.Cursor)\n\
         await _typed(cur).execute(_GET)\n\
         return cur"
    );
}

#[test]
fn a_slice_is_one_placeholder_per_item_and_a_subquery_of_none_when_empty() {
    let mut query = query(Command::Exec, vec![field("ids", Type::INT)]);
    query.sql.0[1] = Piece::Slice {
        marker: "/*SLICE:ids*/".into(),
        placeholder: "?".into(),
        number: 1,
        name: "ids".into(),
    };
    let body = Backend::Pymysql.body(&query, false);
    let body = python::unparse_stmts(&body);
    assert!(
        body.contains(
            r#"cur.execute(_GET.replace("/*SLICE:ids*/%s", ", ".join(["%s"] * len(ids)) or "SELECT NULL FROM DUAL WHERE FALSE"), (*ids,))"#
        ),
        "{body}"
    );
}

#[test]
fn a_one_returns_none_without_a_row_and_raises_on_a_second_by_default() {
    let one = Command::One(crate::api::queries::Shape::Scalar(field("n", Type::INT).ty));
    let query = Query {
        raises: false,
        ..query(one, Vec::new())
    };
    let returns = Backend::Sqlite3.returns(&query, false);
    let body = python::unparse_stmts(&Backend::Sqlite3.body(&query, false));
    assert_eq!(python::unparse(&returns), "int | None");
    assert!(
        body.contains(
            "case []:\n            return None\n        \
             case _:\n            raise MultipleRowsError"
        ),
        "{body}"
    );
}

#[test]
fn a_copy_quotes_every_name_as_postgresql_stores_it() {
    let into = CopyInto {
        schema: "Sales".into(),
        table: "line \"items\"".into(),
        columns: vec!["id".into(), "Unit Price".into()],
    };
    assert_eq!(
        copy(&into),
        r#"COPY "Sales"."line ""items""" ("id", "Unit Price") FROM STDIN"#
    );
    let unqualified = CopyInto {
        schema: String::new(),
        ..into
    };
    assert!(copy(&unqualified).starts_with(r#"COPY "line ""items""" ("#));
}
