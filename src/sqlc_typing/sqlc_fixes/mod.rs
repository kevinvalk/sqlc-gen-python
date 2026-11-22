//! Workarounds for sqlc's reported types, parameters and statement text.

pub(crate) mod bindings;
pub(super) mod parameters;
pub(super) mod types;

use std::collections::HashSet;

use anyhow::{Result, anyhow};

use crate::engine::Engine;
use crate::plugin;

pub(crate) fn checked_queries(
    engine: Engine,
    queries: &[plugin::Query],
) -> impl Iterator<Item = Result<&plugin::Query>> {
    let mut shifted = HashSet::new();
    queries.iter().filter_map(move |query| {
        // sqlc cuts SQLite statements by bytes at character offsets (https://github.com/sqlc-dev/sqlc/issues/4235).
        if engine == Engine::Sqlite {
            if shifted.contains(&query.filename) {
                return None;
            }
            if !query.text.is_ascii() || query.comments.iter().any(|comment| !comment.is_ascii()) {
                shifted.insert(&query.filename);
                return Some(Err(anyhow!(
                    "{}: query \"{}\": sqlc misplaces every SQLite statement after non-ASCII text in its file, so keep the file's SQL and comments ASCII",
                    query.filename, query.name
                )));
            }
        }
        Some(Ok(query))
    })
}
