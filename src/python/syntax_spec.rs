//! Specification for the syntax generated code is built from.

use super::*;
use crate::python::vocabulary::{CONN, CUR, DATE, DICT, INT, LIST, ROW, STR};

#[test]
fn a_string_literal_is_quoted_to_avoid_escaping() {
    // Data is built rather than parsed precisely so ruff escapes it; here it
    // switches quote style instead, as Black does.
    assert_eq!(unparse(&string("open")), r#""open""#);
    assert_eq!(
        unparse(&string(r#"a "quoted" value"#)),
        r#"'a "quoted" value'"#
    );
}

#[test]
fn a_class_without_bases_omits_the_parentheses() {
    let built = class("Author", [], [], [annotate("id", INT)]);
    assert_eq!(unparse_stmt(&built), "class Author:\n    id: int");
}

#[test]
fn an_empty_class_body_becomes_pass() {
    // Python has no empty suite, so something has to stand in for one.
    assert_eq!(
        unparse_stmt(&class("Empty", [], [], [])),
        "class Empty:\n    pass"
    );
}

#[test]
fn a_public_name_is_an_ascii_identifier_that_is_neither_a_keyword_nor_private() {
    for name in ["id", "user_id", "VALUE_0", "Author", "match", "type"] {
        assert!(is_public_name(name), "{name}");
    }
    // Not ASCII: ruff PLC2401, and Python NFKC-normalises identifiers, so
    // `ﬁle` (a ligature) and `file` would be one name.
    for name in [
        "", "1st", "user id", "a-b", "class", "None", "_id", "__x", "_sunder_", "naïve", "ﬁle",
    ] {
        assert!(!is_public_name(name), "{name}");
    }
}

#[test]
fn an_expression_grows_as_python_reads() {
    let written = |expr: ast::Expr| unparse(&expr);
    assert_eq!(written(DATE.into()), "dt.date");
    assert_eq!(
        written(
            CUR.attr("execute")
                .call_with([name("sql")], [keyword("returning", true_())])
        ),
        "cur.execute(sql, returning=True)"
    );
    assert_eq!(written(LIST.index(INT).or_none()), "list[int] | None");
    assert_eq!(
        written(DICT.index_each(vec![STR.into(), INT.into()])),
        "dict[str, int]"
    );
    assert_eq!(
        written(name("text").slice(int(1), minus(int(1)))),
        "text[1:-1]"
    );
    assert_eq!(
        written(
            string(", ")
                .attr("join")
                .call([list(vec![string("?")]).times(name("n"))])
        ),
        r#"", ".join(["?"] * n)"#
    );
    assert_eq!(written(name("a").or(name("b"))), "a or b");
    assert_eq!(
        written(unless_none(name("v"), name("f").call([name("v")]))),
        "None if v is None else f(v)"
    );
    assert_eq!(
        written(list_of(name("x"), each("x", name("xs")).when(name("x")))),
        "[x for x in xs if x]"
    );
    assert_eq!(
        written(forward(LIST.index(name("Mood")))),
        r#""list[Mood]""#
    );
}

#[test]
fn a_coroutine_awaits_and_enters_what_a_blocking_function_calls_and_enters() {
    let body = |asynchronous| {
        let fetched = CUR.attr("fetchall").call([]).awaited(asynchronous);
        let rows = each(ROW, CUR.attr("results").call([])).asynchronous(asynchronous);
        unparse_stmts(&[with(
            asynchronous,
            vec![CONN.attr("cursor").call([]).entered_as(CUR)],
            vec![for_(rows, vec![return_(fetched)])],
        )])
    };
    assert_eq!(
        body(false),
        "with conn.cursor() as cur:\n    for row in cur.results():\n        return cur.fetchall()"
    );
    assert_eq!(
        body(true),
        "async with conn.cursor() as cur:\n    async for row in cur.results():\n        return await cur.fetchall()"
    );
}

#[test]
fn a_statement_is_built_whole() {
    let function: ast::Stmt = Function {
        decorators: vec![name("classmethod")],
        parameters: vec![receiver("cls"), parameter("row", name("int"))],
        keyword_only: vec![parameter("id", name("int"))],
        body: stmts![
            Some(docstring("Doc.")),
            vec![ann_assign("value", INT, ROW)],
            if_(name("value"), vec![raise(name("NoRowError"))]),
        ],
        is_async: true,
        ..Function::new("get", none())
    }
    .into();
    assert_eq!(
        unparse_stmt(&function),
        "@classmethod\nasync def get(cls, row: int, *, id: int) -> None:\n    \"Doc.\"\n    \
         value: int = row\n    if value:\n        raise NoRowError"
    );

    let matched = match_(
        name("rows"),
        vec![
            (
                pattern::sequence(vec![pattern::capture("row")]),
                vec![return_(name("row"))],
            ),
            (pattern::sequence(vec![]), vec![return_(none())]),
            (pattern::wildcard(), vec![raise(name("MultipleRowsError"))]),
        ],
    );
    assert_eq!(
        unparse_stmt(&matched),
        "match rows:\n    case [row]:\n        return row\n    case []:\n        return None\n    \
         case _:\n        raise MultipleRowsError"
    );
    assert_eq!(
        unparse_stmt(&assign_to(CUR.attr("row_factory"), none())),
        "cur.row_factory = None"
    );
}

#[test]
fn an_import_binds_a_module_an_alias_or_names() {
    assert_eq!(
        unparse_stmt(&import("datetime", Some("dt"))),
        "import datetime as dt"
    );
    assert_eq!(unparse_stmt(&import("app.types", None)), "import app.types");
    assert_eq!(
        unparse_stmt(&import_from("typing", &["Any", "Final"])),
        "from typing import Any, Final"
    );
    assert_eq!(
        unparse_stmt(&import_from(".", &["models"])),
        "from . import models"
    );
}
