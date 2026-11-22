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
    result: &crate::sqlc_typing::QueryResult,
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
    let spellings: Vec<&str> = (query.columns.iter().zip(&embeds))
        .map(|(column, embed)| embed.map_or(resolver.spelling(&column.name), |model| model.name.as_str()))
        .collect();

    let mut fields = Vec::new();
    let mut flat = Vec::new();
    let names = names::names(&spellings, Repeats::Numbered, Binding::Field, resolver.config.records)?;
    for ((name, embed), (reported, facts)) in names
        .into_iter()
        .zip(embeds)
        .zip(query.columns.iter().zip(&result.columns))
    {
        match embed {
            Some(model) => {
                // sqlc reports every embed nullable, as its table may be missing unless the statement proves each row
                // has a row of it.
                let table = reported.embed_table.as_ref().map_or("", |t| t.name.as_str());
                let is_outer = !facts.table_present;
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
                let field = resolver.field(name, reported, facts)?;
                flat.push(field.clone());
                fields.push(RowField::Column(field));
            }
        }
    }
    Ok((fields, flat))
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
    // A model lacks the fields of a table's columns that have a problem, so we count the table's own columns: a row of
    // the others is not the table's.
    let columns_of = |model: &Model| {
        (catalog.schema(&model.table.schema)?.tables.iter())
            .find(|table| table.rel.as_ref().is_some_and(|rel| rel.name == model.table.name))
            .map(|table| table.columns.len())
    };
    // Fewer than two columns never get here, as one column is a scalar and a lone embed is no column of the table
    let is_model = |model: &&Model| {
        (columns.iter()).all(|c| (c.table.as_ref()).is_some_and(|t| t.is(&model.table, catalog)))
            && model.fields.len() == fields.len()
            && columns_of(model) == Some(fields.len())
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
