//! What the golden tests and the corpus sweep share: reading fixtures, the
//! invariants every generated output must satisfy, and handing output to
//! `cargo xtask` for the static gate.

mod fixtures;
mod models;
mod module;
mod python;
mod queries;
mod records;

use std::collections::BTreeSet;
use std::path::{Component, Path};

use ruff_python_ast::Stmt;
use ruff_python_parser::parse_module;
use sqlc_gen_python::config::Config;
use sqlc_gen_python::plugin;

pub use fixtures::{Case, case, cases, export, gen_dir, generated_files, read_json, root};
pub use module::check_module;

use models::models_follow_catalog;
use module::{module_problems, prelude};
use queries::{queries_follow_sqlc, stem};

/// Every invariant of what `request` generated, as listed in `corpus.rs`;
/// empty if it holds them all.
pub fn check_output(request: &plugin::GenerateRequest, files: &[plugin::File]) -> Vec<String> {
    let request = &renamed(request);
    let mut problems = Vec::new();
    let stems: BTreeSet<String> = (request.queries.iter())
        .map(|query| stem(&query.filename))
        .collect();
    let expected: BTreeSet<String> = ["__init__.py", "models.py"]
        .map(String::from)
        .into_iter()
        .chain(stems.iter().map(|stem| format!("{stem}.py")))
        .collect();
    let written: BTreeSet<String> = files.iter().map(|file| file.name.clone()).collect();
    if written != expected {
        problems.push(format!(
            "wrote {written:?}, not __init__, models and one module per query file: {expected:?}"
        ));
    }
    let mut names = BTreeSet::new();
    let mut modules = Vec::new();
    for plugin::File { name, contents } in files {
        if !names.insert(name) {
            problems.push(format!("{name}: written twice"));
        }
        if !is_inside_out(name) {
            problems.push(format!("{name}: not a path inside the output directory"));
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
    for (name, source, parsed) in &modules {
        let found = module_problems(parsed, source);
        problems.extend(found.into_iter().map(|p| format!("{name}: {p}")));
    }
    for stem in stems {
        let file = format!("{stem}.py");
        if let Some((module, source)) = body(&file) {
            let mut found = Vec::new();
            let prelude = prelude(module, source);
            queries_follow_sqlc(request, &stem, (module, prelude), models, &mut found);
            problems.extend(found.into_iter().map(|p| format!("{file}: {p}")));
        }
    }
    models_follow_catalog(request, models, &mut problems);
    problems
}

/// `request` with each column and parameter its options `rename` named
/// otherwise spelt as renamed, its SQL name kept as its `original_name`:
/// what the naming checks hold the output to.
fn renamed(request: &plugin::GenerateRequest) -> plugin::GenerateRequest {
    let mut request = request.clone();
    let Ok(config) = Config::from_request(&request) else {
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
        schema
            .tables
            .iter_mut()
            .flat_map(|t| &mut t.columns)
            .for_each(rename);
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

/// Whether a generated file name stays inside the output directory.
fn is_inside_out(name: &str) -> bool {
    let path = Path::new(name);
    path.components().next().is_some()
        && path.components().all(|c| matches!(c, Component::Normal(_)))
}
