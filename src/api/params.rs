use std::collections::HashMap;

use anyhow::{Result, bail};
use heck::ToSnakeCase;

use super::models::{Field, FieldType};
use super::names::{self, Binding, Repeats};
use super::resolver::Resolver;
use crate::engine::Engine;
use crate::plugin;

#[cfg(test)]
#[path = "params_spec.rs"]
mod spec;

/// A query's parameters in sqlc's order, one per placeholder. A name the user chose stays one parameter wherever it is
/// used, also on MySQL where sqlc reports it once per use.
pub(super) fn params(query: &plugin::Query, resolver: &Resolver) -> Result<Vec<Field>> {
    let columns = crate::sqlc_typing::parameter_columns(resolver.engine, query)?;

    // sqlc's Go codegen numbers parameters that spell one name, but a name the user chose (`sqlc.arg`, `@name`) must
    // bind the placeholder it was written at, so it never takes or gives a suffix. Every later use of such a name holds
    // the index of its first use.
    let mut chosen: HashMap<String, usize> = HashMap::new();
    let repeats: Vec<Option<usize>> = (columns.iter().enumerate())
        .map(|(i, column)| {
            if !column.is_named_param || column.name.is_empty() {
                return None;
            }
            let first = *chosen.entry(column.name.clone()).or_insert(i);
            (first != i).then_some(first)
        })
        .collect();
    let mut seen: HashMap<String, &plugin::Column> = HashMap::new();
    for (column, _) in
        (columns.iter().zip(&repeats)).filter(|(column, repeat)| !column.name.is_empty() && repeat.is_none())
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
        .map(|(i, (column, _))| (i, resolver.spelling(&column.name)))
        .collect();
    let spellings: Vec<&str> = firsts.iter().map(|&(_, spelling)| spelling).collect();
    let mut names: Vec<String> = vec![String::new(); columns.len()];
    for ((i, _), name) in firsts.iter().zip(names::names(
        &spellings,
        Repeats::Numbered,
        Binding::Parameter {
            asynchronous: resolver.config.asynchronous,
        },
        resolver.config.records,
    )?) {
        names[*i] = name;
    }
    for (i, repeat) in repeats.iter().enumerate() {
        if let Some(first) = repeat {
            names[i] = names[*first].clone();
        }
    }

    let mut fields: Vec<Field> = Vec::new();
    for (((name, column), parameter), repeat) in names.into_iter().zip(columns).zip(&query.params).zip(&repeats) {
        let what = format!(
            "parameter {} (\"{name}\")",
            resolver.engine.placeholder(parameter.number)
        );
        if column.is_sqlc_slice && resolver.engine == Engine::PostgreSql {
            bail!("sqlc.slice is MySQL/SQLite only; on PostgreSQL use = ANY(@{name}::type[])")
        }

        let mut ty = resolver.field_type(&column, Some(&name), &what, &crate::sqlc_typing::Column::default())?;
        if column.is_sqlc_slice {
            // A slice is a list and never NULL, as we bind each of its items. No engine with slices has arrays, so it
            // is never a list of lists.
            ty.array_dims = 1;
            ty.optional = false;
        }

        // Every use must take one type, and one use that admits NULL makes the parameter optional, as sqlc reports each
        // use's own column. We compare the annotations, as an override naming `str` is the `str` of the type map.
        if let Some(first) = repeat.map(|first| &mut fields[first]) {
            let annotation = |ty: &FieldType| {
                (FieldType {
                    optional: false,
                    ..ty.clone()
                })
                .describe(crate::python::Scope::Queries)
            };
            let (before, now) = (annotation(&first.ty), annotation(&ty));
            if before != now {
                bail!("{what} is used as {before} and as {now}; cast each use to one type");
            }
            if first.ty.element.converters() != ty.element.converters() {
                bail!("{what} has different conversions at its uses; give each use its own parameter");
            }
            first.ty.optional |= ty.optional;
        }
        fields.push(Field {
            name,
            comment: String::new(),
            ty,
        });
    }
    Ok(fields)
}
