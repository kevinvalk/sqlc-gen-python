//! What a query's rows become: its columns as fields, and the model or
//! `<Query>Row` they make.

use anyhow::{Context, Result};

use super::models::{Field, Model};
use super::names::{self, Repeats};
use super::queries::{RowField, Shape};
use super::resolver::Resolver;
use crate::plugin;

#[cfg(test)]
#[path = "results_spec.rs"]
mod spec;

/// A result's fields, and its columns as the database returns them.
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
            (models
                .iter()
                .find(|model| table.is(&model.table, resolver.catalog)))
            .map(Some)
            .with_context(|| format!("sqlc.embed({}) names no table", table.name))
        })
        .collect::<Result<_>>()?;
    let (columns, present) = reported(query, resolver, &embeds);
    let spellings: Vec<&str> = (columns.iter().zip(&embeds))
        .map(|(column, embed)| {
            embed.map_or(resolver.spelling(&column.name), |model| model.name.as_str())
        })
        .collect();

    let mut fields = Vec::new();
    let mut flat = Vec::new();
    for ((name, column), embed) in names::names(&spellings, Repeats::Numbered)?
        .into_iter()
        .zip(&columns)
        .zip(embeds)
    {
        match embed {
            Some(model) => {
                // sqlc reports every embed nullable: its table may be missing
                // unless the statement proves each row has a row of it.
                let table = column.embed_table.as_ref().map_or("", |t| t.name.as_str());
                let outer = !present.iter().any(|name| name.eq_ignore_ascii_case(table));
                let missing = match outer {
                    false => None,
                    true => Some(
                        (model.fields.iter().position(|field| !field.ty.optional)).with_context(
                            || {
                                format!(
                                    "sqlc.embed({table}) of a table that may be missing, as \
                                     after an outer join, of whose columns none is NOT NULL to \
                                     tell a missing row by; select its columns instead"
                                )
                            },
                        )?,
                    ),
                };
                flat.extend(model.fields.iter().cloned().map(|mut field| {
                    field.ty.optional |= outer;
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

/// sqlc's columns of a result, corrected where its report is not enough
/// ([`crate::engine::select`]), and the tables every row has a row of: a
/// `count(...)`'s type is the engine's whatever it counts, which sqlc
/// reports as a guess like any call's; and a scalar subquery that finds no
/// row, and an aggregate of no rows, are NULL, which sqlc reports NOT NULL.
///
/// A statement whose list is not read proves nothing: each of its columns
/// may be NULL, and each table missing.
fn reported(
    query: &plugin::Query,
    resolver: &Resolver,
    embeds: &[Option<&Model>],
) -> (Vec<plugin::Column>, Vec<String>) {
    // An embed is one column of the report, and each of its table's in the
    // statement's list.
    let widths = embeds
        .iter()
        .map(|embed| embed.map(|model| model.fields.len()));
    let width = widths.clone().map(|width| width.unwrap_or(1)).sum();
    let Some(items) = resolver.engine.items(&query.text, width) else {
        let nullable = |column: &plugin::Column| plugin::Column {
            not_null: false,
            ..column.clone()
        };
        return (query.columns.iter().map(nullable).collect(), Vec::new());
    };

    let mut at = 0;
    let columns = (query.columns.iter().zip(widths))
        .map(|(column, embed)| {
            let Some(item) = items.get(at).filter(|_| embed.is_none()) else {
                at += embed.unwrap_or(1);
                return column.clone();
            };
            at += 1;
            let count = item.count && column.is_func_call;
            plugin::Column {
                is_func_call: column.is_func_call && !count,
                r#type: match count {
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

/// The shape of a row of several columns: the model of the table they are
/// exactly the columns of, as sqlc-gen-go reuses one (at least two columns,
/// all of one table, with that table's names, order, types and nullability),
/// or else a `<Query>Row`.
pub(super) fn row_shape(
    models: &[Model],
    columns: &[plugin::Column],
    fields: Vec<RowField>,
    catalog: &plugin::Catalog,
    class: String,
) -> Shape {
    // Fewer than two columns never get here: one is a scalar, and one embed
    // is no column of the table.
    let is_model = |model: &&Model| {
        (columns.iter()).all(|c| (c.table.as_ref()).is_some_and(|t| t.is(&model.table, catalog)))
            && model.fields.len() == fields.len()
            && (model.fields.iter().zip(&fields)).all(|(a, b)| match b {
                RowField::Column(b) => a.name == b.name && a.ty == b.ty,
                RowField::Embed { .. } => false,
            })
    };
    match models.iter().find(is_model) {
        Some(model) => Shape::Model(model.name.clone()),
        None => Shape::Row(class, fields),
    }
}
