//! A sqlc codegen plugin that generates Python.
//!
//! sqlc parses the schema and queries, then hands a plugin a `GenerateRequest`
//! describing everything it resolved. This crate turns that into Python source
//! through five modules, each knowing one thing:
//!
//! - [`plugin`] — the sqlc wire protocol.
//! - [`config`] — what the user asked for.
//! - [`engine`] — which Python type a column type maps to, per database engine.
//! - [`backend`] — the database access layer generated code is written against.
//! - [`schema`] — the catalog resolved into named classes and typed fields.
//! - [`python`] — writing Python: syntax, types, imports, and rendering.
//!
//! [`emit`] assembles files out of them, and `main.rs` is only the stdio shim
//! that wraps [`generate`].

pub mod backend;
pub mod config;
pub mod emit;
pub mod engine;
pub mod plugin;
pub mod python;
pub mod schema;

use anyhow::Result;

/// Generates every file a sqlc codegen entry asked for.
pub fn generate(request: &plugin::GenerateRequest) -> Result<Vec<plugin::File>> {
    let config = config::Config::from_request(request)?;
    let schema = schema::Schema::resolve(request, &config)?;
    emit::files(&schema, &config, request)
}
