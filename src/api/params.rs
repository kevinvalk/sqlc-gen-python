use std::collections::HashMap;

use anyhow::{Result, bail};
use heck::ToSnakeCase;

use super::models::{Field, FieldType};
use super::names::{self, Binding, Repeats};
use super::resolver::Resolver;
use crate::engine::{Bind, Engine, Sql};
use crate::plugin;
use crate::problem::Problem;

#[cfg(test)]
#[path = "params_spec.rs"]
mod spec;

/// We keep one Python argument per chosen name and record which argument binds each SQL placeholder.
pub(super) fn resolve(query: &plugin::Query, resolver: &Resolver) -> Result<(Vec<Field>, Sql)> {
    let columns = crate::sqlc_typing::parameter_columns(resolver.engine, query)?;

    // sqlc's Go codegen numbers parameters that spell one name, but a name the user chose (`sqlc.arg`, `@name`) must
    // bind the placeholder it was written at, so it never takes or gives a suffix. Every later use of such a name holds
    // the index of its first use.
    let mut chosen = HashMap::new();
    let mut spellings = Vec::new();
    let mut arguments = Vec::with_capacity(columns.len());
    let mut seen: HashMap<String, &plugin::Column> = HashMap::new();
    for column in &columns {
        let argument = if column.is_named_param && !column.name.is_empty() {
            *chosen.entry(&column.name).or_insert(spellings.len())
        } else {
            spellings.len()
        };
        arguments.push(argument);
        if argument < spellings.len() {
            continue;
        }
        let spelling = resolver.spelling(&column.name);
        spellings.push(spelling);
        if column.name.is_empty() {
            continue;
        }
        let name = spelling.to_snake_case();
        if let Some(first) = seen.insert(name.clone(), column)
            && (first.is_named_param || column.is_named_param)
        {
            bail!(Problem::new(
                format!(
                    "parameters \"{}\" and \"{}\" both become the Python name \"{name}\"",
                    first.name, column.name
                ),
                "rename one",
            ));
        }
    }

    let names = names::names(
        &spellings,
        Repeats::Numbered,
        Binding::Parameter {
            asynchronous: resolver.config.asynchronous,
        },
        resolver.config.records,
    )?;
    let mut fields: Vec<Field> = Vec::with_capacity(names.len());
    let mut binds = Vec::with_capacity(columns.len());
    for (position, ((column, parameter), argument)) in columns.iter().zip(&query.params).zip(arguments).enumerate() {
        let name = &names[argument];
        binds.push(Bind {
            number: crate::sqlc_typing::bind_number(resolver.engine, position, parameter),
            argument,
            name,
            slice: column.is_sqlc_slice.then_some(column.name.as_str()),
        });
        let what = format!(
            "parameter {} (\"{name}\")",
            resolver.engine.placeholder(parameter.number)
        );
        if column.is_sqlc_slice && resolver.engine == Engine::PostgreSql {
            bail!(Problem::new(
                "sqlc.slice is MySQL/SQLite only",
                format!("on PostgreSQL use = ANY(@{name}::type[])")
            ))
        }

        let mut ty = resolver.field_type(column, Some(name), &what, &crate::sqlc_typing::Column::default())?;
        if column.is_sqlc_slice {
            // A slice is a list and never NULL, as we bind each of its items. No engine with slices has arrays, so it
            // is never a list of lists.
            ty.array_dims = 1;
            ty.optional = false;
        }

        // Every use must take one type, and one use that admits NULL makes the parameter optional, as sqlc reports each
        // use's own column. We compare the annotations, as an override naming `str` is the `str` of the type map.
        if let Some(first) = fields.get_mut(argument) {
            let annotation = |ty: &FieldType| {
                (FieldType {
                    optional: false,
                    ..ty.clone()
                })
                .describe(crate::python::Scope::Queries)
            };
            let (before, now) = (annotation(&first.ty), annotation(&ty));
            if before != now {
                bail!(Problem::new(
                    format!("{what} is used as {before} and as {now}"),
                    "cast each use to one type"
                ));
            }
            if first.ty.element.converters() != ty.element.converters() {
                bail!(Problem::new(
                    format!("{what} has different conversions at its uses"),
                    "give each use its own parameter"
                ));
            }
            first.ty.optional |= ty.optional;
            continue;
        }
        fields.push(Field {
            name: name.clone(),
            comment: String::new(),
            ty,
        });
    }
    Ok((fields, resolver.engine.sql(&query.text, &binds)?))
}
