//! A sqlc codegen plugin that generates Python.
//!
//! sqlc parses the schema and queries, then hands a plugin a `GenerateRequest`
//! describing everything it resolved. [`generate`] turns that into Python:
//!
//! - [`api`] decides the generated API once, whatever the engine and backend:
//!   models, enums, names, functions, parameters and result shapes.
//! - [`render`] writes it as Python files, asking the backend for each
//!   statement and function body.
//! - [`engine`] is the database: which Python type each column type is, and
//!   where its SQL's placeholders are. PostgreSQL today.
//! - [`backend`] is the Python library the generated code runs SQL through.
//!   SQLAlchemy today.
//!
//! Beside them, [`plugin`] is the sqlc wire protocol, [`config`] what the user
//! asked for, and [`python`] how Python is written and rendered. `main.rs` is
//! only the stdio shim that wraps [`generate`].

pub mod api;
pub mod backend;
pub mod config;
pub mod engine;
pub mod plugin;
pub mod python;
pub mod render;

use anyhow::Result;

/// Generates every file a sqlc codegen entry asked for.
pub fn generate(request: &plugin::GenerateRequest) -> Result<Vec<plugin::File>> {
    let config = config::Config::from_request(request)?;
    let api = api::resolve(request, &config)?;
    Ok(render::files(&api, &config, &request.sqlc_version))
}
