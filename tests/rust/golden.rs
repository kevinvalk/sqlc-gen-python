//! Each `codegen` entry of an example's `sqlc.yaml` (`examples/*/`) is a golden case: its options, and its `out`
//! directory holding the expected files. We run them on what sqlc itself sent for each example (`tests/sqlc/examples/`,
//! from `cargo xtask capture`), so these are exactly the requests the plugin sees in production, without sqlc or a WASM
//! runtime in the loop. Reviewing a change means reading the diff of the generated files, the artifact that matters.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use similar_asserts::SimpleDiff;
use sqlc_gen_python::generate;

use crate::support;

type Files = BTreeMap<String, String>;

#[test]
fn every_case_matches_its_expected_output() {
    let mut changed = Vec::new();
    let mut diffs = Vec::new();
    for case in support::cases() {
        if let Some(diff) = diff(&case.directory(), &generate_case(&case)) {
            let example = case.example.file_name().unwrap().to_string_lossy();
            diffs.push(format!("examples/{example}/{}: {diff}", case.name));
            changed.push(format!("{example}/{}", case.name));
        }
    }

    // `cargo xtask bless` rewrites exactly the cases we list here (`<example>/<case>`), once the generated Python
    // passes the static gate.
    if let Some(directory) = support::gen_dir() {
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("changed"), changed.join("\n")).unwrap();
        if std::env::var_os("SQLC_EXPORT_DIFFERENCES").is_some() {
            return;
        }
    }

    assert!(
        diffs.is_empty(),
        "\n{}\n\nRun `cargo xtask bless` if the new output is correct.\n",
        diffs.join("\n\n")
    );
}

/// Anything but a difference from the expected files panics, as `bless` cannot fix a missing fixture or a generator
/// error.
fn generate_case(case: &support::Case) -> Files {
    let request = case.request();
    let directory = case.directory();
    let files = generate(&request).unwrap_or_else(|error| panic!("{directory:?} does not generate: {error:#}"));
    let problems = support::check_output(&request, &files);
    assert!(problems.is_empty(), "{directory:?}:\n  {}", problems.join("\n  "));

    // A case is exported as the package it is, as another case's `models.module` may import it.
    support::export(&case.name, &files);

    (files.into_iter())
        .map(|file| (file.name, String::from_utf8(file.contents).unwrap()))
        .collect()
}

fn diff(case_directory: &Path, generated: &Files) -> Option<String> {
    let expected: Files = (support::generated_files(case_directory).into_iter())
        .map(|file| (file.name, String::from_utf8(file.contents).unwrap()))
        .collect();
    if !generated.keys().eq(expected.keys()) {
        return Some(format!(
            "generated {:?}, expected {:?}",
            generated.keys().collect::<Vec<_>>(),
            expected.keys().collect::<Vec<_>>(),
        ));
    }
    let diffs: Vec<String> = (generated.iter())
        .filter(|(name, contents)| expected[*name] != **contents)
        .map(|(name, contents)| {
            let diff = SimpleDiff::from_str(&expected[name], contents, "expected", "generated");
            format!("{name} differs:\n{diff}")
        })
        .collect();
    (!diffs.is_empty()).then(|| diffs.join("\n"))
}

/// The static gate's `ruff format --check` holds the output to the formatter the plugin renders with, so both must be
/// the same ruff release. Renovate bumps them in one pull request, and this test makes sure it stays that way.
#[test]
fn the_gate_formats_with_the_ruff_the_plugin_renders_with() {
    let read = |path: &str| -> toml::Table {
        toml::from_str(&fs::read_to_string(support::root().join(path)).unwrap()).unwrap()
    };
    let cargo = read("Cargo.toml");
    let tags: std::collections::BTreeSet<&str> = (["dependencies", "dev-dependencies"].iter())
        .flat_map(|section| cargo[*section].as_table().into_iter().flatten())
        .filter(|(name, _)| name.starts_with("ruff_"))
        .filter_map(|(_, dependency)| dependency.get("tag")?.as_str())
        .collect();
    let pyproject = read("tests/python/pyproject.toml");
    let pin = (pyproject["dependency-groups"]["dev"].as_array().into_iter().flatten())
        .find_map(|requirement| requirement.as_str()?.strip_prefix("ruff=="))
        .expect("tests/python/pyproject.toml pins ruff");
    assert_eq!(tags, [pin].into(), "Cargo.toml's ruff crates and the gate's ruff");
}

/// Every tool of the gate reads the oldest Python from `requires-python`, and the plugin sorts imports for it.
#[test]
fn the_plugin_sorts_imports_for_the_python_the_gate_checks() {
    let pyproject: toml::Table =
        toml::from_str(&fs::read_to_string(support::root().join("tests/python/pyproject.toml")).unwrap()).unwrap();
    let requires = pyproject["project"]["requires-python"].as_str().unwrap();
    assert_eq!(requires, format!(">=3.{}", sqlc_gen_python::python::OLDEST_MINOR));
}

/// The runtime scenario reads a NULL address. Once sqlc agrees we can retire the corresponding CTE inference.
#[test]
fn pinned_sqlc_reports_the_nullable_contact_address_as_required() {
    let request = support::case("invoicing_psycopg").request();
    let query = request
        .queries
        .iter()
        .find(|query| query.name == "GetCustomerContact")
        .unwrap();
    let address = query
        .columns
        .iter()
        .find(|column| column.name == "contact_address")
        .unwrap();
    assert!(
        address.not_null,
        "sqlc now reports the nullable CTE address. Review the projection workaround"
    );
}

/// Both occurrences bind the same Python argument. The MySQL scenario exercises the filter with a name and with NULL.
#[test]
fn pinned_sqlc_gives_only_one_use_of_the_mysql_name_filter_a_type() {
    let request = support::case("billing_pymysql").request();
    let query = request
        .queries
        .iter()
        .find(|query| query.name == "FilterCustomers")
        .unwrap();
    let types: Vec<_> = query
        .params
        .iter()
        .filter_map(|param| param.column.as_ref())
        .filter(|column| column.name == "name")
        .map(|column| column.data_type())
        .collect();
    assert_eq!(
        types,
        ["any", "varchar"],
        "sqlc changed the named parameter types. Review parameter_columns"
    );
}
