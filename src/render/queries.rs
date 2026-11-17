use ruff_python_ast as ast;

use super::{Attribute, documentation, record};
use crate::api::models::Field;
use crate::api::queries::{Command, Module, Query, RowField, Takes};
use crate::config::{Config, Records};
use crate::python::Type;
use crate::python::vocabulary::{
    ASYNC_GENERATOR, ASYNC_ITERATOR, CONNECTION, CONTEXTLIB, FINAL, FROM_ROW, GENERATOR, ITERABLE, ITERATOR, MODELS,
    PARAMS, PYDANTIC, ROW, ROWS, Symbol,
};
use crate::python::{Expression, Function, Scope, ann_assign, int, name, none, parameter, stmts, unless_none};

/// For each query its statement, its class if it has one, and its function.
pub fn module(module: &Module, config: &Config) -> Vec<ast::Stmt> {
    let queries = (module.queries.iter()).flat_map(|query| {
        let parameters = (query.takes.class()).map(|class| params_class(class, &query.params, config.records));
        let row = query
            .row()
            .map(|(class, fields)| row_class(class, fields, config.records));
        // `Final` keeps the statement a literal, which psycopg's `execute` asks for (`LiteralString`)
        let statement = ann_assign(&query.constant, FINAL, config.backend.statement(query));
        let functions = functions(query, config);
        std::iter::once(statement).chain(parameters).chain(row).chain(functions)
    });
    (config.backend.prelude(config.asynchronous).into_iter())
        .chain(queries)
        .collect()
}

/// `<Query>Row`, where an embed takes its model's columns.
fn row_class(class: &str, fields: &[RowField], records: Records) -> ast::Stmt {
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
                        // Its first NOT NULL column is only NULL when an outer join left the table without a row
                        Some(column) => (model.or_none(), unless_none(ROW.index(int(start + column)), read)),
                    };
                    Attribute {
                        name: name.clone(),
                        annotation,
                        comment: String::new(),
                        read: Some(read),
                        default: None,
                    }
                }
            }
        })
        .collect();
    record(class, "", &attributes, records)
}

/// `<Query>Params`: one row of a `:copyfrom` or a batch, or the one record of a query's parameters.
///
/// pydantic would turn a JSON document's `True` or `1` into the `float` of its annotation, so it hands one to the
/// encoder as it is.
fn params_class(class: &str, parameters: &[Field], records: Records) -> ast::Stmt {
    let attributes: Vec<Attribute> = (parameters.iter())
        .map(|parameter| Attribute {
            name: parameter.name.clone(),
            annotation: match (records, &parameter.ty.element) {
                (Records::Pydantic, Type::Json(_)) => PYDANTIC
                    .attr("SkipValidation")
                    .index(parameter.ty.parameter_annotation()),
                _ => parameter.ty.parameter_annotation(),
            },
            comment: String::new(),
            read: None,
            default: optional(parameter),
        })
        .collect();
    record(class, "", &attributes, records)
}

/// A nullable parameter may be left out, as `None`: it is how a query says a filter has no constraint
/// (`sqlc.narg(ids) IS NULL OR ...`) or a partial update leaves a column as it is (`coalesce(sqlc.narg(name), name)`),
/// so a caller passes only what it has, `**` a dict of it included.
fn optional(parameter: &Field) -> Option<ast::Expr> {
    parameter.ty.optional.then(none)
}

/// The query's function, and its stream if it has one. Each takes the connection, then each parameter keyword-only, or
/// all of them as one record, or the rows it runs for.
fn functions(query: &Query, config: &Config) -> Vec<ast::Stmt> {
    let (backend, asynchronous) = (config.backend, config.asynchronous);
    let (positional, keyword_only) = match &query.takes {
        Takes::Record(class) => (Some(parameter(PARAMS, name(class))), vec![]),
        Takes::Rows(class) => (Some(parameter(ROWS, ITERABLE.index(name(class)))), vec![]),
        Takes::Keywords => {
            let keywords = (query.params.iter())
                .map(|query_parameter| ast::ParameterWithDefault {
                    default: optional(query_parameter).map(Box::new),
                    ..parameter(&query_parameter.name, query_parameter.ty.parameter_annotation())
                })
                .collect();
            (None, keywords)
        }
    };
    let connection = parameter(CONNECTION, backend.connection(asynchronous));
    let function = Function {
        parameters: std::iter::once(connection).chain(positional).collect(),
        keyword_only,
        body: stmts![documentation(&query.comment), backend.body(query, asynchronous)],
        is_async: asynchronous,
        ..Function::new(&query.function, backend.returns(query, asynchronous))
    };

    // A context manager, so the cursor closes when the caller is done with the rows, read or not
    let stream = match (&query.stream, &query.command) {
        (Some(stream), Command::Many(shape)) => Some((stream, shape)),
        _ => None,
    };
    let stream = stream.map(|(stream, shape)| {
        // The generator spelt out in full, as `contextlib` asks for one and one type argument needs Python 3.13
        let rows = |iterator: Symbol| iterator.index(shape.annotation(false));
        let (decorator, returns) = match asynchronous {
            true => (
                "asynccontextmanager",
                ASYNC_GENERATOR.index_each(vec![rows(ASYNC_ITERATOR), none()]),
            ),
            false => (
                "contextmanager",
                GENERATOR.index_each(vec![rows(ITERATOR), none(), none()]),
            ),
        };
        Function {
            name: stream.clone(),
            decorators: vec![CONTEXTLIB.attr(decorator)],
            returns,
            body: stmts![
                documentation(&query.comment),
                backend.stream(query, (stream, shape), asynchronous)
            ],
            parameters: function.parameters.clone(),
            keyword_only: function.keyword_only.clone(),
            is_async: asynchronous,
        }
        .into()
    });
    std::iter::once(function.into()).chain(stream).collect()
}
