//! Specification for quasi-quoting Python with holes.

use crate::python::{self, py_expr, py_stmts};

fn source(stmts: &[ruff_python_ast::Stmt]) -> String {
    python::render(stmts, &[])
}

#[test]
fn a_hole_takes_an_expression_or_a_name_where_one_stands() {
    let value = python::name("row");
    let expr = py_expr!(
        "None if $v is None else $class($v)",
        v = value,
        class = "Mood"
    );
    assert_eq!(
        python::python_source(&expr),
        "None if row is None else Mood(row)"
    );
    // Data is a node, never parsed: a string holding Python stays a string.
    let sql = py_expr!("cur.execute($sql)", sql = python::string("x) or (1"));
    assert_eq!(python::python_source(&sql), r#"cur.execute("x) or (1")"#);
}

#[test]
fn a_hole_after_star_or_two_takes_a_list_and_a_statement_of_its_own_statements() {
    let stmts = py_stmts!(
        "
        def $name(conn, *, $params) -> int:
            $body
            return cls(*$args, **$fields)
        ",
        name = "get",
        params = vec![python::parameter("id", python::name("int"))],
        body = py_stmts!("x = 1\ny = 2"),
        args = vec![python::int(1), python::int(2)],
        fields = vec![python::keyword("a", python::name("x"))],
    );
    assert_eq!(
        source(&stmts),
        "def get(conn, *, id: int) -> int:\n    x = 1\n    y = 2\n    return cls(1, 2, a=x)\n"
    );
    // No parameters: no `*`.
    let none = py_stmts!(
        "def f(conn, *, $params) -> None: ...",
        params = Vec::<ruff_python_ast::ParameterWithDefault>::new()
    );
    assert_eq!(source(&none), "def f(conn) -> None: ...\n");
}

#[test]
#[should_panic(expected = "holes used and given differ")]
fn a_hole_not_given_is_a_bug() {
    py_expr!("$a + $b", a = python::int(1));
}
