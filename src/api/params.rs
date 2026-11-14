//! A query's parameters, as its function takes them.

use std::collections::HashMap;

use anyhow::{Result, bail};
use heck::ToSnakeCase;

use super::models::Field;
use super::names::{self, Repeats};
use super::resolver::Resolver;
use crate::engine::Engine;
use crate::plugin;
use crate::python::Type;

#[cfg(test)]
#[path = "params_spec.rs"]
mod spec;

/// A query's parameters, in sqlc's order, one per placeholder: a name the
/// user chose names one parameter wherever it is used, which on MySQL sqlc
/// reports once per use.
pub(super) fn params(params: &[plugin::Parameter], resolver: &Resolver) -> Result<Vec<Field>> {
    let columns: Vec<plugin::Column> = (params.iter())
        .map(|param| param.column.clone().unwrap_or_default())
        .collect();
    // A name the user chose is the identifier after `@`. sqlc names the
    // operand of `@` after its parts when it is an expression, which `@`
    // takes when an operator binds tighter (`@day::date AT TIME ZONE 'UTC'`
    // is `@(pg_catalog.timezone('UTC', day::date))`), and may then have
    // replaced all of it with the placeholder.
    if let Some(column) = (columns.iter()).find(|c| {
        c.is_named_param
            && (!resolver.engine.is_identifier(&c.name) || c.name.starts_with("pg_catalog"))
    }) {
        bail!(
            "sqlc named a parameter \"{}\" after an expression, not a name: an operator that \
             binds tighter than @, such as AT TIME ZONE or +, made @ take the expression, and \
             sqlc may have replaced all of it; write sqlc.arg(name), or parenthesise (@name::type)",
            column.name
        );
    }
    // sqlc's Go codegen numbers parameters that spell one name, but a name
    // the user chose (`sqlc.arg`, `@name`) must bind the placeholder it was
    // written at, not take, or give another, a suffix. Each use of one after
    // the first: which use it repeats.
    let mut chosen: HashMap<String, usize> = HashMap::new();
    let repeats: Vec<Option<usize>> = (columns.iter().enumerate())
        .map(|(i, c)| match c.is_named_param && !c.name.is_empty() {
            true => match chosen.get(&c.name) {
                Some(&first) => Some(first),
                None => chosen.insert(c.name.clone(), i).and(None),
            },
            false => None,
        })
        .collect();
    let mut seen: HashMap<String, &plugin::Column> = HashMap::new();
    for (column, _) in
        (columns.iter().zip(&repeats)).filter(|(c, r)| !c.name.is_empty() && r.is_none())
    {
        let name = resolver.spelling(&column.name).to_snake_case();
        if let Some(first) = seen.insert(name.clone(), column)
            && (first.is_named_param || column.is_named_param)
        {
            bail!(
                "parameters \"{}\" and \"{}\" both become the Python name \"{name}\"; rename one",
                first.name,
                column.name,
            );
        }
    }
    let firsts: Vec<(usize, &str)> = (columns.iter().zip(&repeats).enumerate())
        .filter(|(_, (_, repeat))| repeat.is_none())
        .map(|(i, (c, _))| (i, resolver.spelling(&c.name)))
        .collect();
    let spellings: Vec<&str> = firsts.iter().map(|&(_, spelling)| spelling).collect();
    let mut names: Vec<String> = vec![String::new(); columns.len()];
    for ((i, _), name) in firsts
        .iter()
        .zip(names::names(&spellings, Repeats::Numbered)?)
    {
        names[*i] = name;
    }
    for (i, repeat) in repeats.iter().enumerate() {
        if let Some(first) = repeat {
            names[i] = names[*first].clone();
        }
    }

    let mut fields: Vec<Field> = Vec::new();
    for (((name, column), param), repeat) in
        names.into_iter().zip(columns).zip(params).zip(&repeats)
    {
        let what = format!(
            "parameter {} (\"{name}\")",
            resolver.engine.placeholder(param.number)
        );
        if column.is_sqlc_slice && resolver.engine == Engine::PostgreSql {
            bail!("sqlc.slice is MySQL/SQLite only; on PostgreSQL use = ANY(@{name}::type[])")
        }
        // The cast goes on the placeholder: a parameter cannot be coalesced.
        let cast = format!(
            "{}, or {} if it can be NULL",
            resolver.engine.cast(&match resolver.engine {
                Engine::PostgreSql => format!("@{name}"),
                _ => format!("sqlc.arg({name})"),
            }),
            resolver.engine.cast(&format!("sqlc.narg('{name}')"))
        );
        let mut ty = resolver.field_type(&column, true, &what, &cast)?;
        if column.is_sqlc_slice {
            // A list, never NULL: its items are each bound. No engine with
            // slices has arrays, so it is a list of no lists.
            ty.array_dims = 1;
            ty.optional = false;
        }
        // Every use must take one type; one that admits NULL makes it
        // optional, as sqlc reports each use's own column.
        if let Some(first) = repeat.map(|first| &mut fields[first]) {
            if (&first.ty.element, first.ty.array_dims) != (&ty.element, ty.array_dims) {
                bail!(
                    "{what} is used as {} and as {}; cast each use to one type",
                    first.ty.describe(crate::python::Scope::Queries),
                    ty.describe(crate::python::Scope::Queries)
                );
            }
            first.ty.optional |= ty.optional;
            ty.optional = first.ty.optional;
        }
        if matches!(ty.element, Type::Json(_)) && ty.array_dims > 0 {
            bail!(
                "{what} is an array of \"{}\", which has no verified Python mapping",
                column.data_type()
            );
        }
        fields.push(Field {
            name,
            comment: String::new(),
            ty,
        });
    }
    Ok(fields)
}
