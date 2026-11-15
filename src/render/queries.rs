//! A query module: what one query file's queries become
//! (`docs/python-output.md` §6).

use ruff_python_ast as ast;

use super::{Attribute, documentation, record};
use crate::api::models::Field;
use crate::api::queries::{Module, Query, RowField, Takes};
use crate::config::Config;
use crate::python::vocabulary::{CONN, FINAL, FROM_ROW, ITERABLE, MODELS, PARAMS, ROW, ROWS};
use crate::python::{
    Expression, Function, Scope, ann_assign, int, name, parameter, stmts, unless_none,
};

/// The module of one query file (§6): what its backend defines before its
/// queries, if anything, then for each query its statement, its class if it
/// has one, and its function, a coroutine if asked for.
pub fn module(module: &Module, config: &Config) -> Vec<ast::Stmt> {
    let queries = (module.queries.iter()).flat_map(|query| {
        let params = (query.takes.class()).map(|class| params_class(class, &query.params));
        let row = query.row().map(|(class, fields)| row_class(class, fields));
        let statement = ann_assign(&query.constant, FINAL, config.backend.statement(query));
        let function = function(query, config);
        std::iter::once(statement)
            .chain(params)
            .chain(row)
            .chain([function])
    });
    (config.backend.prelude(config.asynchronous).into_iter())
        .chain(queries)
        .collect()
}

/// `<Query>Row`, whose fields are the result's, an embed taking its model's
/// columns.
fn row_class(class: &str, fields: &[RowField]) -> ast::Stmt {
    let mut offset = 0;
    let attributes: Vec<Attribute> = (fields.iter())
        .map(|field| {
            let start = offset;
            match field {
                RowField::Column(field) => {
                    offset += 1;
                    Attribute::column(field, start, Scope::Queries)
                }
                RowField::Embed {
                    name,
                    model,
                    width,
                    missing,
                } => {
                    offset += width;
                    let model = MODELS.attr(model);
                    let columns = ROW.slice(int(start), int(offset));
                    let read = model.clone().attr(FROM_ROW).call([columns]);
                    let (annotation, read) = match missing {
                        None => (model, read),
                        // An outer join left the table without a row.
                        Some(at) => (
                            model.or_none(),
                            unless_none(ROW.index(int(start + at)), read),
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

/// `<Query>Params`: one row of a `:copyfrom` or a batch, or the one record
/// of a query's parameters.
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
    let (backend, asynchronous) = (config.backend, config.asynchronous);
    // The record or the rows after `conn`, or each parameter as a keyword.
    let (positional, keyword_only) = match &query.takes {
        Takes::Record(class) => (Some(parameter(PARAMS, name(class))), vec![]),
        Takes::Rows(class) => (Some(parameter(ROWS, ITERABLE.index(name(class)))), vec![]),
        Takes::Keywords => {
            let keywords = (query.params.iter())
                .map(|p| parameter(&p.name, p.ty.annotation(Scope::Queries)))
                .collect();
            (None, keywords)
        }
    };
    let conn = parameter(CONN, backend.connection(asynchronous));
    Function {
        parameters: std::iter::once(conn).chain(positional).collect(),
        keyword_only,
        body: stmts![
            documentation(&query.comment),
            backend.body(query, asynchronous),
        ],
        is_async: asynchronous,
        ..Function::new(&query.function, backend.returns(query, asynchronous))
    }
    .into()
}
