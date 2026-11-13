//! The generated API, decided once whatever the engine and backend: the
//! models and enums of the catalog ([`models`]), named as sqlc names them
//! ([`inflection`]), and the functions of the queries ([`queries`]). It holds
//! no Python syntax and no backend's SQL; [`crate::render`] writes it.

pub mod inflection;
pub mod models;
pub mod queries;

use std::collections::BTreeSet;

use anyhow::{Context, Result, bail};

use crate::config::Config;
use crate::engine::Engine;
use crate::plugin;
use crate::python::Type;

/// Everything a codegen entry generates, decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Api {
    pub enums: Vec<models::Enum>,
    pub models: Vec<models::Model>,
    pub modules: Vec<queries::Module>,
}

/// The API of `request`, as `config` asks for it. The backend is consulted
/// only for what it cannot represent, which must fail generation.
pub fn resolve(request: &plugin::GenerateRequest, config: &Config) -> Result<Api> {
    let resolver = models::Resolver::new(request, config)?;
    if !config.backend.runs(resolver.engine) {
        bail!(
            "the {} backend does not run {}",
            config.backend.name(),
            resolver.engine.name()
        );
    }
    if config.database_analysis {
        if resolver.engine != Engine::PostgreSql {
            bail!("database_analysis: sqlc analyses queries on a database for PostgreSQL only");
        }
        // Static analysis reports any arithmetic as a bare `int`, which
        // PostgreSQL never names a type.
        let columns = (request.queries.iter()).flat_map(|query| {
            (query.columns.iter()).chain(query.params.iter().filter_map(|p| p.column.as_ref()))
        });
        if let Some(column) = columns.into_iter().find(|c| c.data_type() == "int") {
            bail!(
                "database_analysis: sqlc analysed the queries statically, as it reports column \
                 \"{}\" as a bare int; set analyzer: {{database: true}} on a database in \
                 sqlc.yaml, or remove the option",
                column.name
            );
        }
    }
    if config.asynchronous
        && let Some(problem) = config.backend.async_problem()
    {
        bail!("async: {problem}");
    }
    let mut problems = Problems::default();
    let (enums, models) = models::resolve(&resolver, &config.models, &mut problems);
    let modules = queries::modules(&request.queries, &resolver, &models, config, &mut problems);
    problems.check()?;
    resolver.check_used()?;
    let api = Api {
        enums,
        models,
        modules,
    };
    // A field reads its annotation in its class's namespace, and a function
    // takes the module's: neither may take the name of a module an override
    // imports.
    let roots: Vec<String> = (config.modules().iter())
        .map(|module| module.split('.').next().unwrap_or_default().to_string())
        .collect();
    let queries = api.modules.iter().flat_map(|module| &module.queries);
    let names = (api.models.iter().flat_map(|model| &model.fields))
        .chain(queries.clone().flat_map(|query| &query.columns))
        .chain(
            queries
                .clone()
                .filter(|query| query.rows.is_some())
                .flat_map(|query| &query.params),
        )
        .map(|field| &field.name)
        .chain(queries.map(|query| &query.function));
    if let Some(name) = names.into_iter().find(|name| roots.contains(name)) {
        bail!("`{name}` would hide the module an override imports; rename it");
    }
    for sql_enum in (api.enums.iter()).filter(|e| api.array_enums().contains(e.name.as_str())) {
        (config.backend.check_array_enum(sql_enum))
            .with_context(|| format!("enum class `{}`", sql_enum.name))?;
    }
    Ok(api)
}

/// Every problem of the schema and queries, so that one run reports them all:
/// sqlc shows a plugin's message only once it fails, and a problem at a time
/// would take a run each.
#[derive(Default)]
pub struct Problems(Vec<String>);

impl Problems {
    /// `result`'s value, or `None` with its error noted under `what`.
    pub fn take<T>(&mut self, what: &str, result: Result<T>) -> Option<T> {
        result
            .map_err(|error| self.0.push(format!("{what}: {error:#}")))
            .ok()
    }

    /// Notes `error`.
    pub fn note(&mut self, error: anyhow::Error) {
        self.0.push(format!("{error:#}"));
    }

    /// An error naming every problem, if there is one.
    pub fn check(self) -> Result<()> {
        match self.0.as_slice() {
            [] => Ok(()),
            [one] => bail!("{one}"),
            all => bail!("{} problems:\n{}", all.len(), all.join("\n")),
        }
    }
}

impl Api {
    /// The enum classes some result reads an array of.
    pub fn array_enums(&self) -> BTreeSet<&str> {
        let models = self.models.iter().flat_map(|model| &model.fields);
        let columns = (self.modules.iter())
            .flat_map(|module| &module.queries)
            .flat_map(|query| &query.columns);
        (models.chain(columns))
            .filter(|field| field.ty.array_dims > 0)
            .filter_map(|field| match &field.ty.element {
                Type::Enum(class) => Some(class.as_str()),
                _ => None,
            })
            .collect()
    }
}
