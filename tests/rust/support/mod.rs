mod fixtures;
mod models;
mod module;
mod python;
mod queries;
mod records;

use std::collections::BTreeSet;

use ruff_python_ast::Stmt;
use ruff_python_parser::parse_module;
use sqlc_gen_python::config::{Config, Records, UnknownType};
use sqlc_gen_python::plugin;

pub use fixtures::{Case, case, cases, export, gen_dir, generated_files, read_json, root};
pub use module::check_module;

use models::models_follow_catalog;
use module::{module_problems, prelude};
use queries::{queries_follow_sqlc, stem};

/// Every invariant that what `request` generated breaks, as `corpus.rs` lists them.
pub fn check_output(request: &plugin::GenerateRequest, files: &[plugin::File]) -> Vec<String> {
    let config = Config::from_request(request).ok();
    let request = &renamed(request, config.as_ref());
    let mut problems = Vec::new();
    let stems: BTreeSet<String> = (request.queries.iter()).map(|query| stem(&query.filename)).collect();
    let expected: BTreeSet<String> = ["models.py"]
        .map(String::from)
        .into_iter()
        .chain(stems.iter().map(|stem| format!("{stem}.py")))
        .collect();
    // Exactly these names, so none can reach outside `out` either, which sqlc refuses
    let written: BTreeSet<String> = files.iter().map(|file| file.name.clone()).collect();
    if written != expected {
        problems.push(format!(
            "wrote {written:?}, not models and one module per query file: {expected:?}"
        ));
    }

    let mut names = BTreeSet::new();
    let mut modules = Vec::new();
    for plugin::File { name, contents } in files {
        if !names.insert(name) {
            problems.push(format!("{name}: written twice"));
        }
        match std::str::from_utf8(contents).map(|source| (source, parse_module(source))) {
            Ok((source, Ok(parsed))) => modules.push((name.as_str(), source, parsed)),
            Ok((_, Err(error))) => problems.push(format!("{name}: does not parse: {error}")),
            Err(error) => problems.push(format!("{name}: not UTF-8: {error}")),
        }
    }

    let body = |file: &str| -> Option<(&[Stmt], &str)> {
        (modules.iter())
            .find(|(name, ..)| *name == file)
            .map(|(_, source, parsed)| (&parsed.syntax().body[..], *source))
    };
    let models = body("models.py").map_or(&[][..], |(models, _)| models);
    // The annotation the user asked for every type we cannot verify is no imprecision of ours, and wherever such a type
    // is read or returned it stands in it (`list[Any]`, `Any | None`); the other profiles hold the same requests strictly
    let accepts_any = config
        .as_ref()
        .is_some_and(|config| config.unknown_type == Some(UnknownType::Any));
    let records = config.as_ref().map_or(Records::Dataclass, |config| config.records);
    for (name, source, parsed) in &modules {
        let found = module_problems(parsed, source, accepts_any, records);
        problems.extend(found.into_iter().map(|p| format!("{name}: {p}")));
    }

    for stem in stems {
        let file = format!("{stem}.py");
        if let Some((module, source)) = body(&file) {
            let mut found = Vec::new();
            let prelude = prelude(module, source);
            queries_follow_sqlc((request, config.as_ref()), &stem, (module, prelude), models, &mut found);
            problems.extend(found.into_iter().map(|p| format!("{file}: {p}")));
        }
    }
    models_follow_catalog((request, config.as_ref()), models, &mut problems);
    problems
}

/// We hold the naming checks to the names after the options' `rename`, so each renamed column and parameter takes its
/// new name here and keeps its SQL name as `original_name`.
fn renamed(request: &plugin::GenerateRequest, config: Option<&Config>) -> plugin::GenerateRequest {
    let mut request = request.clone();
    let Some(config) = config else {
        return request;
    };
    let rename = |column: &mut plugin::Column| {
        if let Some(name) = config.rename.get(&column.name) {
            if column.original_name.is_empty() {
                column.original_name = column.name.clone();
            }
            column.name = name.clone();
        }
    };
    for schema in request.catalog.iter_mut().flat_map(|c| &mut c.schemas) {
        schema.tables.iter_mut().flat_map(|t| &mut t.columns).for_each(rename);
    }
    for query in &mut request.queries {
        query.columns.iter_mut().for_each(rename);
        query
            .params
            .iter_mut()
            .filter_map(|p| p.column.as_mut())
            .for_each(rename);
    }
    request
}
