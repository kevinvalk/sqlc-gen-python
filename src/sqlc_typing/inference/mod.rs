//! We read the SQL only for result metadata that sqlc omits or misreports.

mod select;

#[cfg(test)]
mod spec;

use anyhow::{Result, bail};

use super::tokens::is;
use super::{Column, QueryResult};
use crate::engine::Engine;
use crate::plugin;
use crate::problem::Problem;

pub(super) fn refine(
    result: &mut QueryResult,
    engine: Engine,
    query: &plugin::Query,
    catalog: &plugin::Catalog,
) -> Result<()> {
    let words = engine.words(&query.text);
    result.read_only = reads(&words);
    result.single_row = matches!(query.cmd.as_str(), ":one" | ":batchone") && select::single_row(engine, &words);
    // sqlc reports an embed as one column, where the statement lists each column of its table. We give those no table,
    // as it is the embed's to tell a missing row of its table.
    let embed_widths = query.columns.iter().map(|column| {
        let table = column.embed_table.as_ref().filter(|t| !t.name.is_empty())?;
        Some(catalog.table(&table.schema, &table.name).map_or(0, |t| t.columns.len()))
    });
    let tables: Vec<String> = (query.columns.iter().zip(embed_widths.clone()))
        .flat_map(|(column, embed)| match (embed, &column.table) {
            (Some(width), _) => vec![String::new(); width],
            (None, Some(table)) if !table.schema.is_empty() => vec![format!("{}.{}", table.schema, table.name)],
            (None, table) => vec![table.as_ref().map_or_else(String::new, |table| table.name.clone())],
        })
        .collect();
    let tables: Vec<&str> = tables.iter().map(String::as_str).collect();
    let Some(items) = select::result_items(&words, engine, &tables, catalog) else {
        // IMPORTANT: Nothing tells which column sqlc typed by one operand then, and sqlc reports a cast one no
        // differently, so we cannot take its type for any of them.
        if holds_operand_typed(&words, &query.text) {
            bail!(Problem::new(
                "has a result list we could not read, so its coalesce, nullif or CASE, which sqlc types by one \
                 operand whatever the others are, may be any of its columns",
                "if a column is named as a keyword, as \"offset\", quote it"
            ));
        }

        if select::is_set_operation(&words) {
            bail!(Problem::new(
                "has a set-operation result list we could not read",
                "select and cast each column explicitly in every branch"
            ));
        }

        for column in &mut result.columns {
            column.may_be_null = true;
        }
        return Ok(());
    };

    let present = if query.columns.iter().any(|column| column.embed_table.is_some()) {
        select::present_tables(&words, engine)
    } else {
        Vec::new()
    };
    let mut at = 0;
    for ((facts, column), embed) in result.columns.iter_mut().zip(&query.columns).zip(embed_widths) {
        if let Some(width) = embed {
            at += width;
            facts.table_present = column
                .embed_table
                .as_ref()
                .is_some_and(|table| present.iter().any(|name| name.eq_ignore_ascii_case(&table.name)));
            continue;
        }
        let item = &items[at];
        at += 1;
        let count = item.count && column.is_func_call;
        // sqlc calls both a literal and decimal arithmetic `int`. We can verify a literal in every branch.
        let type_name = if count {
            Some(count_type(engine))
        } else if item.integer && column.data_type() == "int" {
            Some(match engine {
                Engine::PostgreSql => "pg_catalog.int4",
                Engine::MySql | Engine::Sqlite => "integer",
            })
        } else {
            None
        };
        *facts = Column {
            type_name,
            verified_call: count,
            problem: item.problem,
            required_elements: item.required_elements,
            may_be_null: item.nullable,
            operand_typed: item.operand_typed,
            ..Column::default()
        };
    }
    Ok(())
}

/// Whether `text` only reads: a `SELECT`, `VALUES` or `TABLE`, or a `WITH` of no statement that writes. A
/// `FOR UPDATE` or `FOR NO KEY UPDATE` only locks what it reads.
fn reads(words: &[String]) -> bool {
    let Some(first) = words.iter().position(|word| word != "(") else {
        return false;
    };
    let words = &words[first..];
    if is(&words[0], &["select", "values", "table"]) {
        return true;
    }
    is(&words[0], &["with"])
        && !words.windows(2).any(|pair| {
            (is(&pair[1], &["insert", "update", "delete", "merge"]) && !is(&pair[0], &["for", "key"]))
                || (is(&pair[0], &["replace"]) && is(&pair[1], &["into"]))
        })
}

/// The type of `count(...)`, spelled as sqlc spells a type it read instead of guessed.
fn count_type(engine: Engine) -> &'static str {
    match engine {
        Engine::PostgreSql => "pg_catalog.int8",
        Engine::Sqlite => "integer",
        Engine::MySql => "bigint",
    }
}

fn holds_operand_typed(words: &[String], text: &str) -> bool {
    let is_typed = |word: &str| {
        ["coalesce", "nullif", "case"]
            .iter()
            .any(|typed| word.eq_ignore_ascii_case(typed))
    };
    match words.is_empty() {
        // MySQL's executable comments leave us without tokens, and what they hold runs, so we look in the text
        true => (text.split(|c: char| !c.is_alphanumeric() && c != '_')).any(is_typed),
        false => words.iter().any(|word| is_typed(word)),
    }
}
