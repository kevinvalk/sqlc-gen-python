use ruff_python_ast::{Expr, StmtFunctionDef};
use sqlc_gen_python::api::names::{Binding, Repeats};
use sqlc_gen_python::plugin;

use super::{Takes, uses};
use crate::support::module::is_none_for_nullable;
use crate::support::python::{Shape, docstring, unparse, words};
use crate::support::records::named_after;

/// Holds one query's function signature and docstring to sqlc's query. Returns whether its parameters pair up with
/// sqlc's, as the checks after it rely on that.
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
        .map(|(_, (parameter, _))| parameter)
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
        .map(|parameter| parameter.column.as_ref().map_or("", |c| c.name.as_str()))
        .collect();
    let what = format!("{}: parameters", query.name);
    named_after(
        &what,
        parameters,
        &spellings,
        Repeats::Numbered,
        Binding::Parameter,
        problems,
    );
    if words(&docstring(function.body.first())) != words(&query.comments.join("\n")) {
        problems.push(format!("{}: its docstring is not its comment", query.name));
    }

    let position = |parameter: &plugin::Parameter| firsts.iter().position(|f| std::ptr::eq(*f, parameter));
    for (parameter, &first) in query.params.iter().zip(&uses) {
        let Some(at) = position(&query.params[first]) else {
            continue;
        };
        let column = parameter.column.clone().unwrap_or_default();

        // A slice is a list of its column's type, and a parameter used twice is optional if either use is.
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
                query.name, parameter.number
            ));
        }
    }
    true
}

/// A query's function is safe to call when it takes `connection` first and positionally, annotated
/// as the backend's connection, then only the `rows` of a `:copyfrom` or batch or the one record `params`, and every
/// other parameter keyword-only. Only a nullable parameter has a default, `None`, so no other can be forgotten, and it is
/// `async def` exactly when the options ask for coroutines.
fn signature_is_safe(
    query: &plugin::Query,
    function: &StmtFunctionDef,
    (connection_annotation, asynchronous, takes): (&str, bool, Takes),
    problems: &mut Vec<String>,
) {
    let parameters = &function.parameters;
    let positional: Vec<&str> = (parameters.posonlyargs.iter().chain(&parameters.args))
        .map(|p| p.name().as_str())
        .collect();
    let expected: &[&str] = match takes {
        Takes::Keywords => &["connection"],
        Takes::Rows => &["connection", "rows"],
        Takes::Record => &["connection", "params"],
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

    let defaults =
        (parameters.iter_non_variadic_params()).filter_map(|p| Some((p.annotation()?, p.default.as_deref()?)));
    if defaults
        .into_iter()
        .any(|(annotation, default)| !is_none_for_nullable(annotation, default))
    {
        problems.push(format!(
            "{}: a parameter has a default, which only a nullable one may, as None",
            query.name
        ));
    }
    let annotation = (parameters.args.first())
        .and_then(|connection| connection.annotation())
        .map(unparse);
    if annotation.as_deref() != Some(connection_annotation) {
        problems.push(format!(
            "{}: takes a connection {annotation:?}, not `{connection_annotation}`",
            query.name
        ));
    }
    if function.is_async != asynchronous {
        problems.push(format!("{}: is `async def` exactly when async", query.name));
    }
}
