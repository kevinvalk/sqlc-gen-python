//! The generated API, decided once, with the engine and the backend asked
//! what only they know: the
//! models and enums of the catalog ([`models`]), typed and spelt by one
//! reading of it ([`resolver`], [`names`]) and named as sqlc names them
//! ([`inflection`]), and the functions of the queries ([`queries`]). A type
//! is held as it is annotated and read; no class, statement or function is
//! built here: [`crate::render`] and the backend write those.

pub mod inflection;
pub mod models;
pub mod names;
mod params;
pub mod queries;
pub mod resolver;
mod results;

#[cfg(test)]
mod fixtures;

use std::collections::BTreeSet;

use anyhow::{Context, Result, bail};

use crate::config::Config;
use crate::plugin;
use crate::python::Type;
use queries::RowField;

/// Everything a codegen entry generates, decided.
#[derive(Debug, Clone, PartialEq)]
pub struct Api {
    pub enums: Vec<models::Enum>,
    pub models: Vec<models::Model>,
    pub modules: Vec<queries::Module>,
}

/// The API of `request`, as `config` asks for it. The backend is consulted
/// only for what it cannot represent, which must fail generation.
pub fn resolve(request: &plugin::GenerateRequest, config: &Config) -> Result<Api> {
    let resolver = resolver::Resolver::new(request, config)?;
    if !config.backend.runs(resolver.engine) {
        bail!(
            "the {} backend does not run {}",
            config.backend.name(),
            resolver.engine.name()
        );
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
    let models = api.models.iter().flat_map(|model| &model.fields);
    let rows = (queries.clone())
        .filter_map(|query| query.row())
        .flat_map(|(_, fields)| fields.iter().map(RowField::name));
    let records = (queries.clone())
        .filter(|query| query.takes.class().is_some())
        .flat_map(|query| &query.params);
    let names = (models.chain(records).map(|field| field.name.as_str()))
        .chain(rows)
        .chain(queries.map(|query| query.function.as_str()));
    if let Some(name) = names
        .into_iter()
        .find(|name| roots.iter().any(|root| root == name))
    {
        bail!(
            "`{name}` would hide the module an override imports; name it otherwise with the \
             rename option"
        );
    }
    let arrays = api.text_array_enums();
    for sql_enum in (api.enums.iter()).filter(|e| arrays.contains(e.name.as_str())) {
        (config.backend.check_array_enum(sql_enum))
            .with_context(|| format!("enum class `{}`", sql_enum.name))?;
    }
    Ok(api)
}

/// Every problem of the schema and queries, so that one run reports them all:
/// sqlc shows a plugin's message only once it fails, and a problem at a time
/// would take a run each.
///
/// A problem says what is wrong where, and after a `; ` what to do about it.
/// Of several, what to do is said once, under the problems it is the fix
/// of: an override types every column of its type.
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
        let all = match self.0.as_slice() {
            [] => return Ok(()),
            [one] => bail!("{one}"),
            all => all,
        };
        // What to do, and the problems it is the fix of, each in the order
        // they were found.
        let mut fixes: Vec<(&str, Vec<&str>)> = Vec::new();
        for problem in all {
            let (wrong, fix) = problem.split_once("; ").unwrap_or((problem, ""));
            match fixes.iter_mut().find(|(known, _)| *known == fix) {
                Some((_, problems)) => problems.push(wrong),
                None => fixes.push((fix, vec![wrong])),
            }
        }
        let lines: Vec<String> = (fixes.iter())
            .flat_map(|(fix, problems)| {
                let fix = (!fix.is_empty()).then(|| format!("  {fix}"));
                problems.iter().map(|wrong| wrong.to_string()).chain(fix)
            })
            .collect();
        bail!("{} problems:\n{}", all.len(), lines.join("\n"))
    }
}

impl Api {
    /// The enum classes some result reads an array of from the array's text.
    pub fn text_array_enums(&self) -> BTreeSet<&str> {
        let models = self.models.iter().flat_map(|model| &model.fields);
        let columns = (self.modules.iter())
            .flat_map(|module| &module.queries)
            .flat_map(|query| &query.columns);
        (models.chain(columns))
            .filter(|field| field.ty.as_text)
            .filter_map(|field| match &field.ty.element {
                Type::Enum(class) => Some(class.as_str()),
                _ => None,
            })
            .collect()
    }
}
