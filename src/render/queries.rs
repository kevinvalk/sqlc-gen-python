//! A query module: what one query file's queries become
//! (`docs/python-output.md` §6).

use ruff_python_ast as ast;

use super::{Attribute, docstring, record};
use crate::api::models::Field;
use crate::api::queries::{Command, Module, Query, RowField, Shape};
use crate::backend::Backend;
use crate::config::Config;
use crate::python::{self, Scope};

/// The module of one query file (§6): for each query, its statement, its
/// class if it has one, and its function, a coroutine if asked for.
pub fn module(module: &Module, config: &Config) -> Vec<ast::Stmt> {
    (module.queries.iter())
        .flat_map(|query| {
            let params = (query.rows.as_ref()).map(|class| params_class(class, &query.params));
            let row = match &query.command {
                Command::One(Shape::Row(class, fields))
                | Command::Many(Shape::Row(class, fields)) => {
                    Some(row_class(class, fields, config.backend.text_arrays()))
                }
                _ => None,
            };
            let statement = python::constant(&query.constant, config.backend.statement(query));
            let function = function(query, config.backend, config.asynchronous);
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
                RowField::Embed { name, model, width } => {
                    offset += width;
                    Attribute {
                        name: name.clone(),
                        annotation: format!("models.{model}"),
                        comment: String::new(),
                        read: Some(format!("models.{model}.from_row(row[{start}:{offset}])")),
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
/// each parameter keyword-only; one execution; the result shaped by `cmd`.
fn function(query: &Query, backend: Backend, asynchronous: bool) -> ast::Stmt {
    let conn = format!("conn: {}", backend.connection(asynchronous));
    let parameters = match (&query.rows, query.params.is_empty()) {
        (Some(class), _) => format!("{conn}, rows: Iterable[{class}]"),
        (None, true) => conn,
        (None, false) => {
            let params = (query.params.iter())
                .map(|p| format!("{}: {}", p.name, p.ty.annotation(Scope::Queries)))
                .collect::<Vec<_>>();
            format!("{conn}, *, {}", params.join(", "))
        }
    };
    let (returns, body) = backend.body(query, asynchronous);

    let def = if asynchronous { "async def" } else { "def" };
    let mut function = python::stmt(&format!(
        "{def} {}({parameters}) -> {returns}: ...",
        query.function
    ));
    let ast::Stmt::FunctionDef(definition) = &mut function else {
        unreachable!("a def is a function")
    };
    definition.body = (docstring(&query.comment).into_iter())
        .chain(python::stmts(&body))
        .collect();
    function
}
