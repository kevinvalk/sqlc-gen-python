//! Golden tests: real sqlc requests in, Python out.
//!
//! Each case under `tests/cases/` names a schema fixture and the plugin options
//! to generate it with, and holds the expected files alongside. The
//! fixtures are captured from sqlc itself (`cargo xtask capture`), so these run
//! against exactly the requests the plugin will see in production, without
//! needing sqlc or a WASM runtime in the loop.
//!
//! Reviewing a change means reading the diff of the generated files, which
//! is the artifact that actually matters. Every generated output must also hold
//! the corpus sweep's invariants ([`support::check_output`]). `cargo xtask
//! bless` rewrites the cases whose output changed, which this test lists in
//! `GEN_DIR/changed`, once the generated Python passes the static gate.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use similar_asserts::SimpleDiff;
use sqlc_gen_python::generate;

use crate::support;

/// The files one case holds or generates, by name.
type Files = BTreeMap<String, String>;

#[test]
fn every_case_matches_its_expected_output() {
    let mut changed = Vec::new();
    let mut diffs = Vec::new();
    for case in cases() {
        let name = case.file_name().unwrap().to_string_lossy().into_owned();
        if let Some(diff) = diff(&case, &generate_case(&format!("cases__{name}"), &case)) {
            diffs.push(format!("tests/cases/{name}: {diff}"));
            changed.push(name);
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

/// Runs the plugin over a case's fixture and options, checks every module's
/// invariants, and hands the output to `cargo xtask` as `package` (see
/// [`support::export`]).
///
/// Anything but a difference from the expected files panics: `bless` cannot
/// fix a missing fixture or a generator error.
fn generate_case(package: &str, case_dir: &Path) -> Files {
    let request = support::case_request(case_dir);
    let files = generate(&request)
        .unwrap_or_else(|error| panic!("{case_dir:?} does not generate: {error:#}"));
    let problems = support::check_output(&request, &files);
    assert!(
        problems.is_empty(),
        "{case_dir:?}:\n  {}",
        problems.join("\n  ")
    );
    support::export(package, &files);

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

/// Every directory under `tests/cases` holding a `case.json`.
fn cases() -> Vec<PathBuf> {
    let dir = support::root().join("tests/cases");
    let mut cases: Vec<PathBuf> = (fs::read_dir(&dir).unwrap())
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.join("case.json").is_file())
        .collect();
    cases.sort();
    assert!(!cases.is_empty(), "no cases found in {dir:?}");
    cases
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
    let pyproject = read("tests/py/pyproject.toml");
    let pin = (pyproject.lines())
        .find_map(|line| line.trim().strip_prefix("\"ruff==")?.strip_suffix("\","))
        .expect("tests/py/pyproject.toml pins ruff");
    assert_eq!(
        tags,
        [pin].into(),
        "Cargo.toml's ruff crates and the gate's ruff"
    );
}
