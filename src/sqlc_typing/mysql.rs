//! sqlc exposes both MySQL SET and ENUM as catalog enums.

use crate::engine::Engine;
use crate::plugin;

#[cfg(test)]
#[path = "mysql_spec.rs"]
mod spec;

/// MySQL makes a catalog enum for a SET's labels. No column of that type holds an enum member.
pub fn is_set(engine: Engine, schema: &plugin::Schema, labels: &plugin::Enum) -> bool {
    if engine != Engine::MySql {
        return false;
    }
    let mut columns = schema
        .tables
        .iter()
        .flat_map(|table| &table.columns)
        .filter(|column| {
            column
                .r#type
                .as_ref()
                .is_some_and(|ty| ty.name == labels.name && (ty.schema.is_empty() || ty.schema == schema.name))
        })
        .peekable();
    columns.peek().is_some() && columns.all(|column| set_length(column, &labels.vals) == Ok(true))
}

/// sqlc reports both SET and ENUM as a list of labels. Its MySQL parser measures a SET by all its labels joined
/// with commas and an ENUM by its longest label, both in bytes.
pub(crate) fn is_set_column(
    engine: Engine,
    catalog: &plugin::Catalog,
    column: &plugin::Column,
    labels: &[String],
) -> Result<bool, (&'static str, &'static str)> {
    if engine != Engine::MySql {
        return Ok(false);
    }
    // sqlc reports -1 for a slice of a SET or ENUM. An omitted protobuf length is zero.
    let column = if column.length <= 0 {
        super::source(catalog, column)
            .filter(|c| c.r#type == column.r#type)
            .unwrap_or(column)
    } else {
        column
    };
    set_length(column, labels)
}

fn set_length(column: &plugin::Column, labels: &[String]) -> Result<bool, (&'static str, &'static str)> {
    if labels.len() == 1 {
        return Err((
            "has one member, which sqlc reports alike for an ENUM and a SET",
            "use a str override or give it a second member",
        ));
    }
    let longest = labels.iter().map(String::len).max().unwrap_or(0);
    let joined = labels.iter().map(String::len).sum::<usize>() + labels.len().saturating_sub(1);
    match usize::try_from(column.length) {
        Ok(length) if length == longest => Ok(false),
        Ok(length) if length == joined => Ok(true),
        _ => Err((
            "has no length that distinguishes its ENUM or SET labels",
            "use a str override",
        )),
    }
}
