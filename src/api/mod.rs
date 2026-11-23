//! We decide the whole generated API here, once: the models and enums of the catalog ([`models`]) and the functions of
//! the queries ([`queries`]), using [`crate::sqlc_typing`] where sqlc misreports their types.
//! Its types spell their own annotations and conversions, but statements and modules are for [`crate::render`] and the
//! backends.

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
use crate::problem::Problem;
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
    let (enums, models) = models::resolve(&resolver, None, catalog_problems);
    // The queries of an entry that writes only the models are a placeholder, as sqlc requires one
    let queries: &[plugin::Query] = if config.models.only { &[] } else { &request.queries };
    let modules = queries::modules(queries, &resolver, &models, config, &mut problems);
    let (enums, models) = match config.models.omit_unused {
        true => models::resolve(&resolver, Some(&used(&modules)), &mut problems),
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
        .filter(|query| {
            query.takes.class().is_some()
                || query
                    .params
                    .iter()
                    .chain(&query.columns)
                    .any(|field| field.ty.element.converters().is_some())
        })
        .flat_map(|query| &query.params);
    let names = (models.chain(records).map(|field| field.name.as_str()))
        .chain(rows)
        .chain(
            queries
                .flat_map(|query| std::iter::once(&query.function).chain(&query.stream))
                .map(String::as_str),
        );
    if let Some(name) = names.into_iter().find(|name| roots.iter().any(|root| root == name)) {
        bail!(
            "`{name}` would hide the module an override imports; name it otherwise with the \
             rename option"
        );
    }

    let arrays = api.text_array_enums();
    for sql_enum in (api.enums.iter()).filter(|e| arrays.contains(e.name.as_str())) {
        crate::backend::check_text_array(sql_enum).with_context(|| format!("enum class `{}`", sql_enum.name))?;
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

/// sqlc shows diagnostics only after generation fails. We report every problem together and print each remedy once.
#[derive(Default)]
pub struct Problems(Vec<Problem>);

impl Problems {
    pub fn take<T>(&mut self, what: &str, result: Result<T>) -> Option<T> {
        result.map_err(|error| self.note(error.context(what.to_owned()))).ok()
    }

    pub fn note(&mut self, error: impl Into<anyhow::Error>) {
        let error = error.into();
        let mut context = Vec::new();
        for cause in error.chain() {
            if let Some(problem) = cause.downcast_ref::<Problem>() {
                context.push(problem.message.clone());
                self.0.push(Problem::new(context.join(": "), &problem.remedy));
                return;
            }
            context.push(cause.to_string());
        }
        self.0.push(Problem::new(context.join(": "), ""));
    }

    pub fn check(self) -> Result<()> {
        match self.0.as_slice() {
            [] => return Ok(()),
            [one] => bail!("{one}"),
            _ => {}
        }
        let mut groups: Vec<(&str, Vec<&str>)> = Vec::new();
        for problem in &self.0 {
            match groups.iter_mut().find(|(remedy, _)| *remedy == problem.remedy) {
                Some((_, messages)) => messages.push(&problem.message),
                None => groups.push((&problem.remedy, vec![&problem.message])),
            }
        }
        let mut message = format!("{} problems:", self.0.len());
        for (remedy, problems) in groups {
            for problem in problems {
                message.push('\n');
                message.push_str(problem);
            }
            if !remedy.is_empty() {
                message.push_str("\n  ");
                message.push_str(remedy);
            }
        }
        bail!("{message}")
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
