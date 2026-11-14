//! What a query's rows become: its columns as fields, and the model or
//! `<Query>Row` they make.

use anyhow::{Context, Result};

use super::models::{Field, Model};
use super::names::{self, Repeats};
use super::queries::{RowField, Shape};
use super::resolver::Resolver;
use crate::engine::select::Item;
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
    // Where sqlc's report is not enough: a `count(...)`'s type is the
    // engine's whatever it counts, which sqlc reports as a guess like any
    // call's; and a scalar subquery is NULL when it finds no row, which sqlc
    // reports NOT NULL.
    let items = resolver.engine.items(&query.text, query.columns.len());
    let columns: Vec<plugin::Column> = (query.columns.iter().enumerate())
        .map(|(i, column)| match items.as_ref().map(|items| &items[i]) {
            Some(Item::Call(name)) if name == "count" && column.is_func_call => plugin::Column {
                is_func_call: false,
                r#type: Some(plugin::Identifier {
                    name: resolver.engine.count_type().into(),
                    ..Default::default()
                }),
                ..column.clone()
            },
            Some(Item::Subquery { row: false }) => plugin::Column {
                not_null: false,
                ..column.clone()
            },
            _ => column.clone(),
        })
        .collect();
    let columns = columns.as_slice();
    let outer = resolver.engine.outer_joined(&query.text);
    let embeds: Vec<Option<&Model>> = (columns.iter())
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
    let spellings: Vec<&str> = (columns.iter().zip(&embeds))
        .map(|(column, embed)| {
            embed.map_or(resolver.spelling(&column.name), |model| model.name.as_str())
        })
        .collect();

    let mut fields = Vec::new();
    let mut flat = Vec::new();
    for ((name, column), embed) in names::names(&spellings, Repeats::Numbered)?
        .into_iter()
        .zip(columns)
        .zip(embeds)
    {
        match embed {
            Some(model) => {
                let table = column.embed_table.as_ref().map_or("", |t| t.name.as_str());
                let outer = (outer.as_ref()).is_none_or(|tables| {
                    tables
                        .iter()
                        .any(|joined| joined.eq_ignore_ascii_case(table))
                });
                let missing = match outer {
                    false => None,
                    true => Some(
                        (model.fields.iter().position(|field| !field.ty.optional)).with_context(
                            || {
                                format!(
                                    "sqlc.embed({table}) of an outer join, of whose columns none \
                                     is NOT NULL to tell a missing row by; select its columns \
                                     instead"
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
