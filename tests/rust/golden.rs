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
            changed.push(case.name);
        }
    }

    // `cargo xtask bless` rewrites exactly the cases we list here, once the generated Python passes the static gate.
    if let Some(directory) = support::gen_dir() {
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("changed"), changed.join("\n")).unwrap();
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

    // NOTE: `cargo xtask bless` copies a case back from this package, so keep the `cases__` prefix in sync with it.
    support::export(&format!("cases__{}", case.name), &files);

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
    let read = |path: &str| fs::read_to_string(support::root().join(path)).unwrap();
    let cargo = read("Cargo.toml");
    let tags: std::collections::BTreeSet<&str> = (cargo.lines())
        .filter(|line| line.starts_with("ruff_"))
        .filter_map(|line| line.split("tag = \"").nth(1)?.split('"').next())
        .collect();
    let pyproject = read("tests/python/pyproject.toml");
    let pin = (pyproject.lines())
        .find_map(|line| line.trim().strip_prefix("\"ruff==")?.strip_suffix("\","))
        .expect("tests/python/pyproject.toml pins ruff");
    assert_eq!(tags, [pin].into(), "Cargo.toml's ruff crates and the gate's ruff");
}
