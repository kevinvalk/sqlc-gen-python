//! What we know about a query beyond the fields in the sqlc request.

mod inference;
mod mysql;
mod sqlc_fixes;
mod tokens;

pub use mysql::is_set;
pub(crate) use mysql::is_set_column;
pub(crate) use sqlc_fixes::parameters::{bind_number, parameter_columns};
pub(crate) use sqlc_fixes::types::{allows_type_override, expression_problem, unmapped_hint};
pub(crate) use sqlc_fixes::{bindings, checked_queries};

use anyhow::Result;

use crate::engine::Engine;
use crate::plugin;

fn source<'a>(catalog: &'a plugin::Catalog, column: &plugin::Column) -> Option<&'a plugin::Column> {
    let table = column.table.as_ref()?;
    catalog
        .table(&table.schema, &table.name)?
        .columns
        .iter()
        .find(|c| c.name == column.original_name)
}

#[derive(Default)]
pub struct QueryResult {
    pub columns: Vec<Column>,
    pub single_row: bool,
    pub read_only: bool,
}

/// Additional facts about one reported column. The default accepts sqlc's type and nullability.
#[derive(Default)]
pub struct Column {
    pub may_be_null: bool,
    required_elements: bool,
    pub(crate) table_present: bool,
    problem: Option<&'static str>,
    operand_typed: bool,
    type_name: Option<&'static str>,
    verified_call: bool,
}

impl Column {
    pub(crate) fn nullable_elements(&self, engine: Engine, dimensions: usize) -> bool {
        // A proof about inputs of array_agg applies to its immediate elements. An input array can itself contain NULL.
        engine == Engine::PostgreSql && dimensions > 0 && (dimensions > 1 || !self.required_elements)
    }

    pub(crate) fn apply(&self, reported: &plugin::Column) -> plugin::Column {
        let mut column = reported.clone();
        if let Some(name) = self.type_name {
            column.r#type = Some(plugin::Identifier {
                name: name.into(),
                ..Default::default()
            });
        }
        column.is_func_call &= !self.verified_call;
        column.not_null &= !self.may_be_null;
        column
    }
}

impl QueryResult {
    pub fn read(engine: Engine, query: &plugin::Query, catalog: &plugin::Catalog) -> Result<Self> {
        let mut result = Self::default();
        if matches!(query.cmd.as_str(), ":one" | ":many" | ":batchone" | ":batchmany") {
            result.columns.resize_with(query.columns.len(), Column::default);
            inference::refine(&mut result, engine, query, catalog)?;
        }
        Ok(result)
    }
}
