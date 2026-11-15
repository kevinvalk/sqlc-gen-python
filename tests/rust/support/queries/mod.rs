//! The invariants holding each query module to sqlc's request: here, pairing
//! each query with its statement and function; in the submodules, what each
//! pair must then say of its signature, its execution and its result.

mod execution;
mod result;
mod signature;

use std::path::Path;

use ruff_python_ast::{Expr, Stmt, StmtClassDef, StmtFunctionDef};
use sqlc_gen_python::backend::Backend;
use sqlc_gen_python::config::Config;
use sqlc_gen_python::engine::Engine;
use sqlc_gen_python::plugin;

use super::module::{ASYNCMY, PYMYSQL};
use super::python::{fields, is_name};
use execution::{runs_its_statement, sql_round_trips};
use result::result_follows_sqlc;
use signature::function_follows_sqlc;

/// Checks that the module of query file `stem` implements sqlc's queries of
/// it (`docs/python-output.md` §6).
///
/// After its `prelude`, which it holds exactly where its library's stubs
/// need one (PyMySQL's, or asyncmy's for the pymysql backend's coroutines),
/// the module holds one statement constant and one function per query, in
/// sqlc's order, so they pair up without repeating any naming rule. Each pair
/// must run exactly the SQL sqlc wrote, and its signature must say what sqlc
/// says of NULL and arrays: a parameter, result field or bare result
/// annotated optional exactly when sqlc says it admits NULL, and a list
/// exactly as deep as its array. No type checker or runtime probe can see a
/// non-null value annotated `X | None`.
pub(super) fn queries_follow_sqlc(
    request: &plugin::GenerateRequest,
    stem: &str,
    (body, prelude): (&[Stmt], Option<(usize, &[&str])>),
    models: &[Stmt],
    problems: &mut Vec<String>,
) {
    let Ok(config) = Config::from_request(request) else {
        return problems.push("generated from options that do not parse".into());
    };
    let (backend, asynchronous) = (config.backend, config.asynchronous);
    let needed = match (backend, asynchronous) {
        (Backend::Pymysql, false) => Some(PYMYSQL),
        (Backend::Pymysql, true) => Some(ASYNCMY),
        (Backend::SqlAlchemy | Backend::Psycopg | Backend::Sqlite3, _) => None,
    };
    if prelude.map(|(_, held)| held) != needed {
        problems.push("its prelude is not the one its library's stubs need".into());
    }
    let body = &body[prelude.map_or(0, |(at, held)| at + held.len())..];
    let Ok(engine) = Engine::of(request) else {
        return problems.push("generated for an engine it does not support".into());
    };
    let queries: Vec<&plugin::Query> = (request.queries.iter())
        .filter(|query| self::stem(&query.filename) == stem)
        .collect();
    let statements: Vec<(&str, String)> = (body.iter())
        .filter_map(|stmt| {
            let constant = stmt.as_ann_assign_stmt()?;
            let sql = statement_sql(constant.value.as_deref()?, backend)?;
            is_name(&constant.annotation, "Final")
                .then_some((constant.target.as_name_expr()?.id.as_str(), sql))
        })
        .collect();
    let functions = functions(body);
    if statements.len() != queries.len() || functions.len() != queries.len() {
        return problems.push(format!(
            "{} statements and {} functions for {} queries",
            statements.len(),
            functions.len(),
            queries.len()
        ));
    }
    let conn = &sqlc_gen_python::python::unparse(&backend.connection(asynchronous));
    for ((query, (constant, sql)), function) in queries.into_iter().zip(statements).zip(functions) {
        let parameters = parameters(body, function);
        let takes = Takes::of(query, &config);
        let signature = (conn.as_str(), asynchronous, takes);
        if function_follows_sqlc(query, function, &parameters, signature, problems) {
            runs_its_statement(
                query,
                function,
                constant,
                &parameters,
                engine,
                (asynchronous, takes),
                problems,
            );
            result_follows_sqlc(
                request,
                query,
                function,
                (body, models),
                (&config, engine),
                problems,
            );
            sql_round_trips(query, &sql, &parameters, backend, engine, problems);
        }
    }
}

/// A module's functions, in order.
fn functions(body: &[Stmt]) -> Vec<&StmtFunctionDef> {
    body.iter().filter_map(Stmt::as_function_def_stmt).collect()
}

/// Each of sqlc's parameters, as the index of the first one it is: a name the
/// user chose (`sqlc.arg`) is one parameter however often sqlc reports it, as
/// MySQL's `?` cannot be bound twice.
fn uses(query: &plugin::Query) -> Vec<usize> {
    let column = |i: usize| query.params[i].column.clone().unwrap_or_default();
    (0..query.params.len())
        .map(|i| {
            let this = column(i);
            (0..i)
                .find(|&j| {
                    let other = column(j);
                    this.is_named_param
                        && other.is_named_param
                        && !this.name.is_empty()
                        && other.name == this.name
                })
                .unwrap_or(i)
        })
        .collect()
}

/// Whether a query's function takes `rows` of its parameters: a `:copyfrom`,
/// or a batch (`:batchexec`, `:batchone`, `:batchmany`).
fn takes_rows(query: &plugin::Query) -> bool {
    query.cmd == ":copyfrom" || query.cmd.starts_with(":batch")
}

/// How a query's function takes its parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Takes {
    /// Each as a keyword.
    Keywords,
    /// `rows` of them.
    Rows,
    /// One record of them, `params`: more than the options' `max_params`.
    Record,
}

impl Takes {
    fn of(query: &plugin::Query, config: &Config) -> Self {
        let uses = uses(query);
        let distinct = (0..uses.len()).filter(|&i| uses[i] == i).count();
        match (takes_rows(query), config.max_params) {
            (true, _) => Self::Rows,
            (false, Some(max)) if distinct > max => Self::Record,
            _ => Self::Keywords,
        }
    }

    /// Where a parameter's value is read: itself, a row's field, the
    /// record's.
    fn value(self, name: &str) -> String {
        match self {
            Self::Keywords => name.to_string(),
            Self::Rows => format!("r.{name}"),
            Self::Record => format!("params.{name}"),
        }
    }
}

/// The module a query file is: its stem, each `-` a `_`.
pub(super) fn stem(filename: &str) -> String {
    (Path::new(filename).file_stem())
        .and_then(|stem| stem.to_str())
        .unwrap_or_default()
        .replace('-', "_")
}

/// The SQL of a statement constant: on SQLAlchemy, the string it wraps in
/// `sqlalchemy.text(...)`; on a driver, the string itself.
fn statement_sql(expr: &Expr, backend: Backend) -> Option<String> {
    match backend {
        Backend::SqlAlchemy => text_of(expr),
        _ => Some(expr.as_string_literal_expr()?.value.to_str().into()),
    }
}

/// The string a statement constant wraps in `sqlalchemy.text(...)`, under any
/// methods called on it.
fn text_of(expr: &Expr) -> Option<String> {
    let call = expr.as_call_expr()?;
    match &*call.func {
        Expr::Attribute(method) if method.attr.as_str() == "text" => {
            let string = call.arguments.args.first()?.as_string_literal_expr()?;
            Some(string.value.to_str().into())
        }
        Expr::Attribute(method) => text_of(&method.value),
        _ => None,
    }
}

/// The names a function binds its query's parameters under, and their
/// annotations: its keyword-only parameters, or the fields of the `Params`
/// class its `rows` are made of.
fn parameters<'a>(body: &'a [Stmt], function: &'a StmtFunctionDef) -> Vec<(&'a str, &'a Expr)> {
    if let Some(class) = params_class(body, function) {
        return fields(class);
    }
    (function.parameters.kwonlyargs.iter())
        .filter_map(|p| Some((p.name().as_str(), p.annotation()?)))
        .collect()
}

/// The `Params` class of the module a function takes `rows` of, or one
/// record of as `params`.
fn params_class<'a>(body: &'a [Stmt], function: &StmtFunctionDef) -> Option<&'a StmtClassDef> {
    let class = (function.parameters.args.iter()).find_map(|p| {
        let annotation = p.parameter.annotation.as_deref()?;
        match p.name().as_str() {
            "rows" => annotation.as_subscript_expr()?.slice.as_name_expr(),
            "params" => annotation.as_name_expr(),
            _ => None,
        }
    })?;
    (body.iter().filter_map(Stmt::as_class_def_stmt)).find(|c| c.name.as_str() == class.id.as_str())
}
