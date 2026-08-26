//! Golden tests: real sqlc requests in, Python out.
//!
//! Each case under `tests/cases/` names a schema fixture and the plugin options
//! to generate it with, and holds the expected `.py` files alongside. The
//! fixtures are captured from sqlc itself (see `make fixtures`), so these run
//! against exactly the requests the plugin will see in production, without
//! needing sqlc or a WASM runtime in the loop.
//!
//! Reviewing a change means reading the diff of the generated `.py` files, which
//! is the artifact that actually matters. Run `make bless` to rewrite them after
//! an intentional change.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use sqlc_gen_python::{generate, plugin};

/// The files one case produces, by name.
type Files = BTreeMap<String, String>;

/// What a case generates, and from what.
#[derive(Debug, Deserialize)]
struct Case {
    /// The schema fixture under `tests/schemas/`, without the extension.
    schema: String,
    /// The plugin options, exactly as they would appear under `options:`.
    #[serde(default)]
    options: serde_json::Value,
}

#[test]
fn every_case_matches_its_expected_output() {
    let failures: Vec<String> = cases()
        .filter_map(|case| {
            check(&case)
                .err()
                .map(|error| format!("{case:?}: {error:#}"))
        })
        .collect();

    assert!(
        failures.is_empty(),
        "\n{}\n\nRun `make bless` if the new output is correct.\n",
        failures.join("\n\n")
    );
}

fn check(case_dir: &Path) -> Result<()> {
    let generated = generate_case(case_dir)?;

    for (name, contents) in &generated {
        // The generator must never emit Python that does not parse. This is
        // cheap, and it catches a class of mistake a golden file would happily
        // record as the new expected output.
        ruff_python_parser::parse_module(contents)
            .with_context(|| format!("generated {name} does not parse"))?;
    }

    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        return bless(case_dir, &generated);
    }

    let expected = read_python(case_dir)?;
    ensure!(
        generated.keys().eq(expected.keys()),
        "generated {:?}, expected {:?}",
        generated.keys().collect::<Vec<_>>(),
        expected.keys().collect::<Vec<_>>(),
    );
    for (name, contents) in &generated {
        similar_asserts::assert_eq!(expected: &expected[name], generated: contents, "{name}");
    }
    Ok(())
}

/// Runs the plugin over a case's fixture and options.
fn generate_case(case_dir: &Path) -> Result<Files> {
    let case: Case = read_json(&case_dir.join("case.json"))?;
    let fixture = root()
        .join("tests/schemas")
        .join(&case.schema)
        .with_extension("json");

    let mut request: plugin::GenerateRequest = read_json(&fixture)?;
    request.plugin_options = serde_json::to_vec(&case.options)?;

    generate(&request)?
        .into_iter()
        .map(|file| Ok((file.name, String::from_utf8(file.contents)?)))
        .collect()
}

/// Rewrites a case's expected output, dropping files no longer generated.
fn bless(case_dir: &Path, generated: &Files) -> Result<()> {
    for stale in read_python(case_dir)?.keys() {
        if !generated.contains_key(stale) {
            fs::remove_file(case_dir.join(stale))?;
        }
    }
    for (name, contents) in generated {
        fs::write(case_dir.join(name), contents)?;
    }
    Ok(())
}

fn read_python(case_dir: &Path) -> Result<Files> {
    fs::read_dir(case_dir)?
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            path.extension().filter(|ext| *ext == "py")?;
            let name = path.file_name()?.to_str()?.to_string();
            Some(fs::read_to_string(&path).map(|contents| (name, contents)))
        })
        .collect::<std::io::Result<Files>>()
        .map_err(Into::into)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let contents = fs::read_to_string(path).with_context(|| format!("reading {path:?}"))?;
    serde_json::from_str(&contents).with_context(|| format!("parsing {path:?}"))
}

/// Every directory under `tests/cases` holding a `case.json`.
fn cases() -> impl Iterator<Item = PathBuf> {
    let dir = root().join("tests/cases");
    let mut cases: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|error| panic!("reading {dir:?}: {error}"))
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| path.join("case.json").is_file())
        .collect();

    cases.sort();
    assert!(!cases.is_empty(), "no cases found in {dir:?}");
    cases.into_iter()
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Re-captures the sqlc requests the cases run against.
///
/// sqlc has a built-in `json` codegen that dumps the exact `GenerateRequest` a
/// plugin receives, so a fixture is real sqlc output rather than something this
/// repository made up. The raw dump is ~2 MB because the catalog carries all of
/// `pg_catalog`; pruning the schemas the plugin ignores anyway leaves something
/// small enough to read in a review and cheap enough to commit.
///
/// Ignored by default because it needs sqlc on `PATH` and only matters after a
/// schema under `tests/schemas/src` changes:
///
/// ```sh
/// cargo test --test golden -- --ignored
/// ```
#[test]
#[ignore = "needs sqlc; run with --ignored to re-capture"]
fn capture_fixtures_from_sqlc() {
    /// Schemas the engine always reports but that no generated model describes.
    const INTERNAL: [&str; 3] = ["pg_catalog", "information_schema", "pg_temp"];

    let sources = root().join("tests/schemas/src");
    for entry in fs::read_dir(&sources).expect("tests/schemas/src") {
        let source = entry.unwrap().path();
        if !source.join("sqlc.json").is_file() {
            continue;
        }

        let status = std::process::Command::new("sqlc")
            .arg("generate")
            .current_dir(&source)
            .status()
            .expect("running sqlc");
        assert!(status.success(), "sqlc failed in {source:?}");

        let dump = source.join("gen/codegen.json");
        let mut request: serde_json::Value = read_json(&dump).unwrap();
        fs::remove_dir_all(dump.parent().unwrap()).unwrap();

        let schemas = &mut request["catalog"]["schemas"];
        *schemas = schemas
            .as_array()
            .unwrap()
            .iter()
            .filter(|schema| !INTERNAL.contains(&schema["name"].as_str().unwrap_or_default()))
            .cloned()
            .collect();

        // Options belong to a case, not to a schema.
        for owned_by_the_case in ["plugin_options", "global_options"] {
            request.as_object_mut().unwrap().remove(owned_by_the_case);
        }

        let fixture = root()
            .join("tests/schemas")
            .join(source.file_name().unwrap())
            .with_extension("json");
        fs::write(&fixture, format!("{:#}\n", request)).unwrap();
        println!("{}", fixture.display());
    }
}
