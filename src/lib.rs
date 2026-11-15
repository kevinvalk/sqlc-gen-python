//! A sqlc codegen plugin that generates Python. `docs/development.md` describes the architecture.

pub mod api;
pub mod backend;
pub mod config;
pub mod engine;
pub mod plugin;
pub mod python;
pub mod render;

use anyhow::Result;

pub fn generate(request: &plugin::GenerateRequest) -> Result<Vec<plugin::File>> {
    let config = config::Config::from_request(request)?;
    let api = api::resolve(request, &config)?;
    Ok(render::files(&api, &config, &request.sqlc_version))
}
