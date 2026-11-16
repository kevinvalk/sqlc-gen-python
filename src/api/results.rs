use anyhow::{Context, Result};

use super::models::{Field, Model};
use super::names::{self, Binding, Repeats};
use super::queries::{RowField, Shape};
use super::resolver::Resolver;
use crate::plugin;

#[cfg(test)]
#[path = "results_spec.rs"]
mod spec;

/// The fields of a result, and its columns as the database returns them, every embed expanded.
pub(super) fn columns(
    query: &plugin::Query,
    resolver: &Resolver,
    models: &[Model],
) -> Result<(Vec<RowField>, Vec<Field>)> {
    let embeds: Vec<Option<&Model>> = (query.columns.iter())
        .map(|column| {
            let Some(table) = column.embed_table.as_ref().filter(|t| !t.name.is_empty()) else {
                return Ok(None);
            };
            (models.iter().find(|model| table.is(&model.table, resolver.catalog)))
                .map(Some)
                .with_context(|| format!("sqlc.embed({}) names no table", table.name))
        })
        .collect::<Result<_>>()?;
    let (columns, present) = reported(query, resolver, &embeds);
    let spellings: Vec<&str> = (columns.iter().zip(&embeds))
        .map(|(column, embed)| embed.map_or(resolver.spelling(&column.name), |model| model.name.as_str()))
        .collect();

    let mut fields = Vec::new();
    let mut flat = Vec::new();
    for ((name, column), embed) in names::names(&spellings, Repeats::Numbered, Binding::Field)?
        .into_iter()
        .zip(&columns)
        .zip(embeds)
    {
        match embed {
            Some(model) => {
                // sqlc reports every embed nullable, as its table may be missing unless the statement proves each row
                // has a row of it.
                let table = column.embed_table.as_ref().map_or("", |t| t.name.as_str());
                let is_outer = !present.iter().any(|name| name.eq_ignore_ascii_case(table));
                let missing = match is_outer {
                    false => None,
                    true => Some(
                        (model.fields.iter().position(|field| !field.ty.optional)).with_context(|| {
                            format!(
                                "sqlc.embed({table}) of a table that may be missing, as \
                                     after an outer join, of whose columns none is NOT NULL to \
                                     tell a missing row by; select its columns instead"
                            )
                        })?,
                    ),
                };
                flat.extend(model.fields.iter().cloned().map(|mut field| {
                    field.ty.optional |= is_outer;
                    field
                }));
                fields.push(RowField::Embed {
                    missing,
                    name,
                    model: model.name.clone(),
                    width: model.fields.len(),
                });
            }
            None => {
                let field = resolver.field(name, column)?;
                flat.push(field.clone());
                fields.push(RowField::Column(field));
            }
        }
    }
    Ok((fields, flat))
}

/// sqlc's columns of a result, corrected where its report is not enough ([`crate::engine::select`]), and the tables
/// every row has a row of. sqlc reports the type of a `count(...)` as a guess like any call's, while it is the engine's
/// whatever it counts. And it reports NOT NULL for a scalar subquery that finds no row, an aggregate of no rows and a
/// column of a table an outer join left without a row, which are all NULL.
///
/// A statement whose list we cannot read proves nothing, so then every column may be NULL and every table missing.
fn reported(
    query: &plugin::Query,
    resolver: &Resolver,
    embeds: &[Option<&Model>],
) -> (Vec<plugin::Column>, Vec<String>) {
    // sqlc reports an embed as one column, where the statement lists each column of its table. We give those no table,
    // as it is the embed's to tell a missing row of its table.
    let tables: Vec<String> = (query.columns.iter().zip(embeds))
        .flat_map(|(column, embed)| match (embed, &column.table) {
            (Some(model), _) => vec![String::new(); model.fields.len()],
            (None, Some(table)) if !table.schema.is_empty() => vec![format!("{}.{}", table.schema, table.name)],
            (None, table) => vec![table.as_ref().map_or_else(String::new, |table| table.name.clone())],
        })
        .collect();
    let tables: Vec<&str> = tables.iter().map(String::as_str).collect();
    let Some(items) = resolver.engine.items(&query.text, &tables) else {
        let nullable = |column: &plugin::Column| plugin::Column {
            not_null: false,
            ..column.clone()
        };
        return (query.columns.iter().map(nullable).collect(), Vec::new());
    };

    let mut at = 0;
    let columns = (query.columns.iter().zip(embeds))
        .map(|(column, embed)| {
            let item = items.get(at).filter(|_| embed.is_none());
            at += embed.map_or(1, |model| model.fields.len());
            let Some(item) = item else {
                return column.clone();
            };
            let is_count = item.count && column.is_func_call;
            plugin::Column {
                is_func_call: column.is_func_call && !is_count,
                r#type: match is_count {
                    true => Some(plugin::Identifier {
                        name: resolver.engine.count_type().into(),
                        ..Default::default()
                    }),
                    false => column.r#type.clone(),
                },
                not_null: column.not_null && !item.nullable,
                ..column.clone()
            }
        })
        .collect();
    (columns, resolver.engine.present(&query.text))
}

/// The shape of a row of several columns: the model of a table when they are exactly its columns (all of that table,
/// with its names, order, types and nullability), as sqlc-gen-go reuses one, or else a `<Query>Row`.
pub(super) fn row_shape(
    models: &[Model],
    columns: &[plugin::Column],
    fields: Vec<RowField>,
    catalog: &plugin::Catalog,
    class: String,
) -> Shape {
    // Fewer than two columns never get here, as one column is a scalar and a lone embed is no column of the table
    let is_model = |model: &&Model| {
        (columns.iter()).all(|c| (c.table.as_ref()).is_some_and(|t| t.is(&model.table, catalog)))
            && model.fields.len() == fields.len()
            && (model.fields.iter().zip(&fields)).all(|(field, row_field)| match row_field {
                RowField::Column(column) => field.name == column.name && field.ty == column.ty,
                RowField::Embed { .. } => false,
            })
    };
    match models.iter().find(is_model) {
        Some(model) => Shape::Model(model.name.clone()),
        None => Shape::Row(class, fields),
    }
}
