//! Golden tests: real sqlc requests in, Python out.
//!
//! Each `codegen` entry of an example's `sqlc.yaml` (`examples/*/`) is a case:
//! its options, and its `out` directory holding the expected files. What sqlc
//! sends for each example is captured from sqlc itself (`tests/sqlc/examples/`,
//! `cargo xtask capture`), so these run against exactly the requests the
//! plugin will see in production, without needing sqlc or a WASM runtime in
//! the loop.
//!
//! Reviewing a change means reading the diff of the generated files, which
//! is the artifact that actually matters. Every generated output must also hold
//! the corpus sweep's invariants ([`support::check_output`]). `cargo xtask
//! bless` rewrites the cases whose output changed, which this test lists in
//! `GEN_DIR/changed`, once the generated Python passes the static gate.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use similar_asserts::SimpleDiff;
use sqlc_gen_python::generate;

use crate::support;

/// The files one case holds or generates, by name.
type Files = BTreeMap<String, String>;

#[test]
fn every_case_matches_its_expected_output() {
    let mut changed = Vec::new();
    let mut diffs = Vec::new();
    for case in support::cases() {
        if let Some(diff) = diff(&case.dir(), &generate_case(&case)) {
            let example = case.example.file_name().unwrap().to_string_lossy();
            diffs.push(format!("examples/{example}/{}: {diff}", case.name));
            changed.push(case.name);
        }
    }
    if let Some(dir) = support::gen_dir() {
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("changed"), changed.join("\n")).unwrap();
    }

    assert!(
        diffs.is_empty(),
        "\n{}\n\nRun `cargo xtask bless` if the new output is correct.\n",
        diffs.join("\n\n")
    );
}

/// Runs the plugin over a case's request and options, checks every module's
/// invariants, and hands the output to `cargo xtask` as `cases__<name>` (see
/// [`support::export`]).
///
/// Anything but a difference from the expected files panics: `bless` cannot
/// fix a missing fixture or a generator error.
fn generate_case(case: &support::Case) -> Files {
    let request = case.request();
    let dir = case.dir();
    let files =
        generate(&request).unwrap_or_else(|error| panic!("{dir:?} does not generate: {error:#}"));
    let problems = support::check_output(&request, &files);
    assert!(problems.is_empty(), "{dir:?}:\n  {}", problems.join("\n  "));
    support::export(&format!("cases__{}", case.name), &files);

    (files.into_iter())
        .map(|file| (file.name, String::from_utf8(file.contents).unwrap()))
        .collect()
}

/// How the generated files differ from the expected ones, if they do.
fn diff(case_dir: &Path, generated: &Files) -> Option<String> {
    let expected: Files = (support::generated_files(case_dir).into_iter())
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

/// The static gate's `ruff format --check` holds the output to the formatter
/// the plugin renders with, so both must be one ruff release. Dependabot bumps
/// the Python pin but cannot bump the crates' git tag.
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
    assert_eq!(
        tags,
        [pin].into(),
        "Cargo.toml's ruff crates and the gate's ruff"
    );
}
