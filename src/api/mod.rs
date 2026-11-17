//! We decide the whole generated API here, once: the models and enums of the catalog ([`models`]) and the functions of
//! the queries ([`queries`]), with every name and type, asking the engine and the backend only what they alone know.
//! Nothing in here builds Python syntax, that is for [`crate::render`] and the backends.

pub mod inflection;
pub mod models;
pub mod names;
mod params;
pub mod queries;
pub mod resolver;
mod results;

#[cfg(test)]
pub(crate) mod fixtures;

use std::collections::BTreeSet;

use anyhow::{Context, Result, bail};

use crate::config::Config;
use crate::plugin;
use crate::python::Type;
use queries::RowField;

#[derive(Debug, Clone, PartialEq)]
pub struct Api {
    pub enums: Vec<models::Enum>,
    pub models: Vec<models::Model>,
    pub modules: Vec<queries::Module>,
}

/// We only ask the backend about what it cannot represent, as that has to fail generation.
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
    // We match the queries against every table, and with `omit_unused` then write only the classes they use, whose
    // problems alone are the user's
    let mut unwritten = Problems::default();
    let catalog_problems = match config.models.omit_unused {
        true => &mut unwritten,
        false => &mut problems,
    };
    let (enums, models) = models::resolve(&resolver, &config.models, None, catalog_problems);
    // The queries of an entry that writes only the models are a placeholder, as sqlc requires one
    let queries: &[plugin::Query] = if config.models.only { &[] } else { &request.queries };
    let modules = queries::modules(queries, &resolver, &models, config, &mut problems);
    let (enums, models) = match config.models.omit_unused {
        true => models::resolve(&resolver, &config.models, Some(&used(&modules)), &mut problems),
        false => (enums, models),
    };
    problems.check()?;
    resolver.check_used()?;
    let api = Api { enums, models, modules };

    // A field resolves its annotation in its class's namespace and a function in its module's, so neither may take the
    // name of a module an override imports.
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
    if let Some(name) = names.into_iter().find(|name| roots.iter().any(|root| root == name)) {
        bail!(
            "`{name}` would hide the module an override imports; name it otherwise with the \
             rename option"
        );
    }

    let arrays = api.text_array_enums();
    for sql_enum in (api.enums.iter()).filter(|e| arrays.contains(e.name.as_str())) {
        (config.backend.check_array_enum(sql_enum)).with_context(|| format!("enum class `{}`", sql_enum.name))?;
    }
    Ok(api)
}

/// The classes of the models module that `modules` use: the models of their rows and the enums of their fields.
fn used(modules: &[queries::Module]) -> BTreeSet<String> {
    let queries = modules.iter().flat_map(|module| &module.queries);
    let enums = (queries.clone())
        .flat_map(|query| query.params.iter().chain(&query.columns))
        .filter_map(|field| match &field.ty.element {
            Type::Enum(class) => Some(class.as_str()),
            _ => None,
        });
    (queries.flat_map(queries::Query::models).chain(enums))
        .map(String::from)
        .collect()
}

/// We collect every problem of the schema and queries so that one run reports them all, as sqlc only shows a plugin's
/// output once it fails and a problem at a time would take a run each.
///
/// A problem says what is wrong where and, after a `; `, what to do about it. We say each fix once, under all the
/// problems it fixes, as one override types every column of its type.
#[derive(Default)]
pub struct Problems(Vec<String>);

impl Problems {
    /// The value of `result`, or `None` after noting its error under `what`.
    pub fn take<T>(&mut self, what: &str, result: Result<T>) -> Option<T> {
        result.map_err(|error| self.0.push(format!("{what}: {error:#}"))).ok()
    }

    pub fn note(&mut self, error: anyhow::Error) {
        self.0.push(format!("{error:#}"));
    }

    /// Fails with every problem we noted, if there is any.
    pub fn check(self) -> Result<()> {
        let all = match self.0.as_slice() {
            [] => return Ok(()),
            [one] => bail!("{one}"),
            all => all,
        };

        // We group the problems by their fix, keeping both in the order we found them.
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
    /// The enum classes of which some field reads an array from the array's text (`from_array`).
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
