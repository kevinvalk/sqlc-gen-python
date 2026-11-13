//! A query module: what one query file's queries become
//! (`docs/python-output.md` §6).

use ruff_python_ast as ast;

use super::{Attribute, docstring, record};
use crate::api::models::Field;
use crate::api::queries::{Command, Module, Query, RowField, Shape};
use crate::config::Config;
use crate::python::{self, Scope, py_expr, py_stmt};

/// The module of one query file (§6): for each query, its statement, its
/// class if it has one, and its function, a coroutine if asked for.
pub fn module(module: &Module, config: &Config) -> Vec<ast::Stmt> {
    (module.queries.iter())
        .flat_map(|query| {
            let params = (query.rows.as_ref().or(query.bundle.as_ref()))
                .map(|class| params_class(class, &query.params));
            let row = match &query.command {
                Command::One(Shape::Row(class, fields))
                | Command::Many(Shape::Row(class, fields)) => {
                    Some(row_class(class, fields, config.backend.text_arrays()))
                }
                _ => None,
            };
            let statement = python::constant(&query.constant, config.backend.statement(query));
            let function = function(query, config);
            std::iter::once(statement)
                .chain(params)
                .chain(row)
                .chain([function])
        })
        .collect()
}

/// `<Query>Row`, whose fields are the result's, an embed taking its model's
/// columns.
fn row_class(class: &str, fields: &[RowField], text_arrays: bool) -> ast::Stmt {
    let mut offset = 0;
    let attributes: Vec<Attribute> = (fields.iter())
        .map(|field| {
            let start = offset;
            match field {
                RowField::Column(field) => {
                    offset += 1;
                    Attribute::column(field, start, Scope::Queries, text_arrays)
                }
                RowField::Embed {
                    name,
                    model,
                    width,
                    missing,
                } => {
                    offset += width;
                    let (rows, end) = (python::int(start), python::int(offset));
                    let model = py_expr!("models.$model", model = model);
                    let read = py_expr!(
                        "$model.from_row(row[$start:$end])",
                        model = model.clone(),
                        start = rows,
                        end = end,
                    );
                    let (annotation, read) = match missing {
                        None => (model, read),
                        // An outer join left the table without a row.
                        Some(at) => (
                            py_expr!("$model | None", model = model),
                            py_expr!(
                                "None if row[$at] is None else $read",
                                at = python::int(start + at),
                                read = read,
                            ),
                        ),
                    };
                    Attribute {
                        name: name.clone(),
                        annotation,
                        comment: String::new(),
                        read: Some(read),
                    }
                }
            }
        })
        .collect();
    record(class, "", &attributes)
}

/// `<Query>Params`, one row of a `:copyfrom` or a batch.
fn params_class(class: &str, params: &[Field]) -> ast::Stmt {
    let attributes: Vec<Attribute> = (params.iter())
        .map(|param| Attribute {
            name: param.name.clone(),
            annotation: param.ty.annotation(Scope::Queries),
            comment: String::new(),
            read: None,
        })
        .collect();
    record(class, "", &attributes)
}

/// The function that runs a query (§6, "Functions"): the connection, then
/// each parameter keyword-only, or all of them as one record, or the rows it
/// runs for; one execution; the result shaped by `cmd`.
fn function(query: &Query, config: &Config) -> ast::Stmt {
    let asynchronous = config.asynchronous;
    let conn = config.backend.connection(asynchronous);
    let (returns, body) = config.backend.body(query, asynchronous);
    let body: Vec<ast::Stmt> = docstring(&query.comment).into_iter().chain(body).collect();
    let mut function = match (&query.rows, &query.bundle) {
        (None, Some(class)) => py_stmt!(
            "
            def $name(conn: $conn, params: $class) -> $returns:
                $body
            ",
            name = &query.function,
            conn = conn,
            class = class,
            returns = returns,
            body = body,
        ),
        (Some(class), _) => py_stmt!(
            "
            def $name(conn: $conn, rows: Iterable[$class]) -> $returns:
                $body
            ",
            name = &query.function,
            conn = conn,
            class = class,
            returns = returns,
            body = body,
        ),
        (None, None) => py_stmt!(
            "
            def $name(conn: $conn, *, $params) -> $returns:
                $body
            ",
            name = &query.function,
            conn = conn,
            params = (query.params.iter())
                .map(|p| python::parameter(&p.name, p.ty.annotation(Scope::Queries)))
                .collect::<Vec<_>>(),
            returns = returns,
            body = body,
        ),
    };
    if let ast::Stmt::FunctionDef(definition) = &mut function {
        definition.is_async = asynchronous;
    }
    function
}
