//! The invariants of a query function's signature: safe to call, and its
//! parameters named, documented and shaped as sqlc's.

use ruff_python_ast::{Expr, StmtFunctionDef};
use sqlc_gen_python::api::names::Repeats;
use sqlc_gen_python::plugin;

use super::{Takes, uses};
use crate::support::python::{Shape, docstring, unparse, words};
use crate::support::records::named_after;

/// Checks one query's function's signature and documentation against sqlc's
/// query; whether its parameters pair up with sqlc's, for the checks after.
pub(super) fn function_follows_sqlc(
    query: &plugin::Query,
    function: &StmtFunctionDef,
    parameters: &[(&str, &Expr)],
    signature: (&str, bool, Takes),
    problems: &mut Vec<String>,
) -> bool {
    signature_is_safe(query, function, signature, problems);
    let uses = uses(query);
    let firsts: Vec<&plugin::Parameter> = (query.params.iter().zip(&uses).enumerate())
        .filter(|(i, (_, first))| i == *first)
        .map(|(_, (param, _))| param)
        .collect();
    if parameters.len() != firsts.len() {
        let names: Vec<&str> = parameters.iter().map(|(name, _)| *name).collect();
        problems.push(format!(
            "{}: parameters {names:?} for {} placeholders",
            query.name,
            firsts.len()
        ));
        return false;
    }
    let spellings: Vec<&str> = (firsts.iter())
        .map(|param| param.column.as_ref().map_or("", |c| c.name.as_str()))
        .collect();
    let what = format!("{}: parameters", query.name);
    named_after(&what, parameters, &spellings, Repeats::Numbered, problems);
    if words(&docstring(function.body.first())) != words(&query.comments.join("\n")) {
        problems.push(format!("{}: its docstring is not its comment", query.name));
    }
    let position = |param: &plugin::Parameter| firsts.iter().position(|f| std::ptr::eq(*f, param));
    for (param, &first) in query.params.iter().zip(&uses) {
        let Some(at) = position(&query.params[first]) else {
            continue;
        };
        let column = param.column.clone().unwrap_or_default();
        // A slice is a list of its column's type; a parameter used twice is
        // optional if either use is.
        let optional = (query.params.iter().zip(&uses))
            .any(|(other, &f)| f == first && !other.column.as_ref().is_some_and(|c| c.not_null));
        let expected = Shape {
            optional,
            depth: column.array_dims as usize + usize::from(column.is_sqlc_slice),
        };
        let (name, annotation) = parameters[at];
        if Shape::of(annotation) != expected {
            problems.push(format!(
                "{}: parameter `{name}` disagrees with sqlc's {} on NULL or array depth",
                query.name, param.number
            ));
        }
    }
    true
}

/// Checks what makes one query's function safe to call (§6, "Functions"):
/// `conn` first and positional, annotated as the backend's connection, then
/// only the `rows` of a `:copyfrom` or batch, or the one record `params`,
/// every other parameter keyword-only; no defaults, so none can be forgotten;
/// and `async def` exactly when the options ask for coroutines.
fn signature_is_safe(
    query: &plugin::Query,
    function: &StmtFunctionDef,
    (conn, asynchronous, takes): (&str, bool, Takes),
    problems: &mut Vec<String>,
) {
    let parameters = &function.parameters;
    let positional: Vec<&str> = (parameters.posonlyargs.iter().chain(&parameters.args))
        .map(|p| p.name().as_str())
        .collect();
    let expected: &[&str] = match takes {
        Takes::Keywords => &["conn"],
        Takes::Rows => &["conn", "rows"],
        Takes::Record => &["conn", "params"],
    };
    if positional != expected
        || !parameters.posonlyargs.is_empty()
        || parameters.vararg.is_some()
        || parameters.kwarg.is_some()
        || (takes != Takes::Keywords && !parameters.kwonlyargs.is_empty())
    {
        problems.push(format!(
            "{}: takes {positional:?} positionally, not {expected:?} and the rest keyword-only",
            query.name
        ));
    }
    if parameters
        .iter_non_variadic_params()
        .any(|p| p.default.is_some())
    {
        problems.push(format!("{}: a parameter has a default", query.name));
    }
    let annotation = (parameters.args.first())
        .and_then(|conn| conn.annotation())
        .map(unparse);
    if annotation.as_deref() != Some(conn) {
        problems.push(format!(
            "{}: takes a connection {annotation:?}, not `{conn}`",
            query.name
        ));
    }
    if function.is_async != asynchronous {
        problems.push(format!("{}: is `async def` exactly when async", query.name));
    }
}
