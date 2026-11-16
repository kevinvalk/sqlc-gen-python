//! The corpus sweep: every request sqlc's own test suite produces (`tests/sqlc/endtoend/`, from `cargo xtask capture`),
//! under every profile of its `profiles.json`, must either generate sound Python or fail cleanly. Nothing here knows
//! what the output should be, the golden tests pin that for a few curated cases. Instead every output must hold what
//! any correct output holds:
//!
//! - generating never panics, and generating twice gives the same result;
//! - the files are exactly `models.py` and one module per query file, each a distinct path inside the output
//!   directory, and each valid UTF-8;
//! - every module parses, and ruff's formatter leaves it unchanged;
//! - its only comments are the header it opens with;
//! - every name a module binds is unique in its scope and public, except for the prelude a query module holds where
//!   its library's stubs need one, to the letter and nowhere else: PyMySQL's connection under a private name, and
//!   asyncmy's untyped methods as a private protocol that annotates with `Any`;
//! - every name a module reads on import is bound by then;
//! - no annotation mentions `Any`, except a driver row's, a psycopg connection's and `:execresult`'s;
//! - every class but an enum and an error is exactly a frozen, slotted, keyword-only dataclass, and only a nullable
//!   field of a record of parameters has a default, `None`;
//! - the models are one enum per catalog enum, with its labels as members in order and each named after its label,
//!   and one dataclass per table, with its columns as fields in order, each optional exactly when the column admits
//!   NULL and a list exactly as deep as its array, and a `from_row` that reads each field from its own position;
//! - each query module holds one statement and one function per query, in sqlc's order. Each function takes
//!   `connection` first, annotated as the backend's connection, then every SQL parameter keyword-only, none with a
//!   default, and is `async def` exactly when asked. Its parameters and result fields are named after sqlc's, and
//!   they (or its bare result) are optional exactly when sqlc says and lists exactly as deep. It executes its own
//!   statement once, binding each parameter as itself (by name, or in its placeholder's place), and does nothing else
//!   with `connection`. A `Row`'s `from_row` reads each field from its own place, an embed from a contiguous slice.
//!   It returns a model exactly when the result is that table's columns, and its statement is exactly sqlc's SQL, as
//!   written for a driver, or after one pass through SQLAlchemy's own bind expressions;
//! - every docstring is the comment of what it documents, and every comment (a table's, column's, enum's or
//!   query's) is a docstring.
//!
//! [`support::check_output`] checks all but the first, which is checked here, and `invariants.rs` plants a bug for
//! each of them.
//!
//! An error is an allowed outcome, as sqlc accepts features we do not support yet, as long as it is an `Err` and not
//! a panic. `tests/sqlc/endtoend/ledger.tsv` records which requests generate and which do not, with a hash of the
//! output, so a change in either shows up in review: this test fails until `cargo xtask bless` rewrites the ledger.
//! Every distinct output is also handed to `cargo xtask check`, which runs the Python toolchain over it.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

use sha2::{Digest, Sha256};
use sqlc_gen_python::{generate, plugin};

use crate::support;

/// A request, relative to `tests/sqlc/endtoend`, and a profile.
type Key = (String, String);

#[test]
fn every_request_generates_sound_python_or_fails_cleanly() {
    let corpus = support::root().join("tests/sqlc/endtoend");
    let profiles: BTreeMap<String, serde_json::Value> = support::read_json(&corpus.join("profiles.json"));

    // We sweep sequentially as the whole corpus takes about half a second in a debug build.
    let mut ledger = BTreeMap::new();
    let mut violations = String::new();

    // Many requests generate the same output, so we export each output once, as the package of the first row that
    // has it, and `packages` maps every row to that package.
    let mut exported = BTreeMap::new();
    let mut packages = String::from("request\tprofile\tpackage\n");
    for path in requests(&corpus) {
        let request: plugin::GenerateRequest = support::read_json(&corpus.join(&path));
        for (profile, options) in &profiles {
            match sweep(&request, options) {
                Ok(Outcome::Generated { sha8, files }) => {
                    let package = exported.entry(sha8.clone()).or_insert_with(|| {
                        let package = package(&path, profile);
                        support::export(&package, &files);
                        package
                    });
                    writeln!(packages, "{path}\t{profile}\t{package}").unwrap();
                    ledger.insert((path.clone(), profile.clone()), format!("ok\t{sha8}"));
                }
                Ok(Outcome::Failed(reason)) => {
                    let outcome = format!("error:{reason}\t-");
                    ledger.insert((path.clone(), profile.clone()), outcome);
                }
                Err(problems) => {
                    for problem in problems {
                        writeln!(violations, "{path} [{profile}]: {problem}").unwrap();
                    }
                }
            }
        }
    }
    assert!(violations.is_empty(), "\n{violations}");

    if let Some(directory) = support::gen_dir() {
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("packages.tsv"), packages).unwrap();
    }
    check_ledger(&corpus.join("ledger.tsv"), &ledger);
}

/// What generating one request under one profile did, breaking no invariant.
enum Outcome {
    Generated { sha8: String, files: Vec<plugin::File> },
    Failed(String),
}

/// Generates `request` under one profile's options, and returns every invariant it broke as the error.
fn sweep(request: &plugin::GenerateRequest, options: &serde_json::Value) -> Result<Outcome, Vec<String>> {
    // We build the request the way sqlc itself sends it: the options, plus the codegen entry they came from.
    let request = plugin::GenerateRequest {
        plugin_options: serde_json::to_vec(options).unwrap(),
        settings: Some(plugin::Settings {
            codegen: Some(plugin::Codegen {
                out: "db".into(),
                plugin: "python".into(),
                ..Default::default()
            }),
            ..request.settings.clone().unwrap_or_default()
        }),
        ..request.clone()
    };
    let run = || {
        catch_unwind(AssertUnwindSafe(|| generate(&request)))
            .map_err(|panic| {
                let message = (panic.downcast_ref::<String>().map(String::as_str))
                    .or_else(|| panic.downcast_ref::<&str>().copied())
                    .unwrap_or("non-string payload");
                vec![format!("panicked: {message}")]
            })
            .map(|result| result.map_err(|error| one_line(&format!("{error:#}"))))
    };

    let first = run()?;
    if run()? != first {
        return Err(vec!["generating twice gave different results".into()]);
    }
    let files = match first {
        Ok(files) => files,
        Err(reason) => return Ok(Outcome::Failed(reason)),
    };

    let problems = support::check_output(&request, &files);
    if !problems.is_empty() {
        return Err(problems);
    }

    let mut digest = Sha256::new();
    for file in &files {
        digest.update(format!("{}\0{}\0", file.name, file.contents.len()));
        digest.update(&file.contents);
    }
    Ok(Outcome::Generated {
        sha8: sha8(digest),
        files,
    })
}

/// Fails if the outcomes differ from the committed ledger, listing every transition and counting them by kind
/// (`ok -> error`, ...). For `cargo xtask bless` we write the new ledger to `GEN_DIR/ledger.tsv`, and the transitions
/// that lose output (`ok -> error`, `... -> (none)`) to `GEN_DIR/regressions`, which bless refuses unless told
/// otherwise.
fn check_ledger(path: &Path, outcomes: &BTreeMap<Key, String>) {
    let rendered: String = std::iter::once("request\tprofile\toutcome\tsha8\n".to_string())
        .chain(
            outcomes
                .iter()
                .map(|((request, profile), outcome)| format!("{request}\t{profile}\t{outcome}\n")),
        )
        .collect();
    let committed: BTreeMap<Key, String> = (fs::read_to_string(path).unwrap_or_default())
        .lines()
        .skip(1)
        .filter_map(|line| {
            let (request, rest) = line.split_once('\t')?;
            let (profile, outcome) = rest.split_once('\t')?;
            Some(((request.into(), profile.into()), outcome.into()))
        })
        .collect();

    let keys: BTreeSet<&Key> = committed.keys().chain(outcomes.keys()).collect();
    let mut transitions = Vec::new();
    let mut regressions = String::new();
    let mut by_kind = BTreeMap::<String, usize>::new();
    for key @ (request, profile) in keys {
        let (before, after) = (committed.get(key), outcomes.get(key));
        if before == after {
            continue;
        }
        let show = |outcome: Option<&String>| outcome.map_or("(none)".into(), |o| o.replace('\t', " "));
        let row = format!("{request} [{profile}]: {} -> {}", show(before), show(after));
        let (from, to) = (kind(before), kind(after));
        if (from == "ok" && to != "ok") || to == "(none)" {
            writeln!(regressions, "{row}").unwrap();
        }
        let changed = if from == to { " (output changed)" } else { "" };
        *by_kind.entry(format!("{from} -> {to}{changed}")).or_default() += 1;
        transitions.push(row);
    }

    if let Some(directory) = support::gen_dir() {
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("ledger.tsv"), &rendered).unwrap();
        fs::write(directory.join("regressions"), &regressions).unwrap();
    }

    let counts: Vec<String> = (by_kind.iter())
        .map(|(kind, count)| format!("{count:>6}  {kind}"))
        .collect();
    assert!(
        transitions.is_empty(),
        "\nthe corpus ledger changed ({} rows):\n{}\n\nby transition:\n{}\n\n\
         Run `cargo xtask bless` if every transition is intended.\n",
        transitions.len(),
        transitions.join("\n"),
        counts.join("\n"),
    );
}

fn kind(outcome: Option<&String>) -> &'static str {
    match outcome {
        None => "(none)",
        Some(outcome) if outcome.starts_with("error") => "error",
        Some(_) => "ok",
    }
}

fn requests(corpus: &Path) -> Vec<String> {
    let manifest = fs::read_to_string(corpus.join("MANIFEST.tsv")).expect("reading MANIFEST.tsv");
    let requests: BTreeSet<String> = (manifest.lines().skip(1))
        .filter_map(|line| line.split('\t').nth(2))
        .filter(|request| request.ends_with(".json"))
        .map(Into::into)
        .collect();
    assert!(!requests.is_empty(), "no requests in {corpus:?}");
    requests.into_iter().collect()
}

/// The package an output is checked as: its request and profile spelled as a Python identifier, so
/// `a/b/request.managed-db.json` under `async` becomes `corpus__a__b__request_managed_db__async`.
fn package(request: &str, profile: &str) -> String {
    let request = request.trim_end_matches(".json").replace('/', "__");
    format!("corpus__{request}__{profile}").replace(['.', '-'], "_")
}

fn one_line(message: &str) -> String {
    message.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn sha8(digest: Sha256) -> String {
    (digest.finalize()[..4].iter())
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
