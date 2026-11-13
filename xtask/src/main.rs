//! Developer commands for this repository, run as `cargo xtask <command>`.
//!
//! The `xtask` convention: a small program of the workspace, which
//! `.cargo/config.toml` runs as `cargo xtask`, instead of a Makefile or shell
//! scripts, so there is no build system beyond cargo. The Python side runs
//! through uv.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

mod sql_text;

const USAGE: &str = "\
usage: cargo xtask <command>

commands:
  check   rustfmt --check, clippy with warnings denied, cargo test, the
          static Python gate over every golden case and corpus output, and
          the scanner proof of the SQL rewriting
  bless [CASE...] [--allow-regressions]
          rewrite the golden cases that changed (the generated directories in
          examples/, each a codegen `out` of its sqlc.yaml) and the corpus
          ledger, or with CASE names only those cases; refuses if the unit specs, the sweep or
          the static gate fail, if anything else would change, or if a corpus
          request stops generating (unless --allow-regressions); a change to
          a case the runtime proof reads back (pgtypes_sqlalchemy, invoicing_*,
          ledger_*)
          also needs verify to pass (docker or SQLC_TEST_DATABASE_URL), which
          rewrites docs/typemap.md and docs/compat.md to match
  capture [--sqlc-bin PATH] [--check]
          re-capture tests/sqlc/endtoend and tests/sqlc/examples/*.json from the sqlc
          .prototools pins (downloaded, or the binary at PATH; several
          minutes), or with --check fail if that would change anything, then
          run the WASM plugin under that sqlc and fail unless it keeps the
          wire contract (writing exactly the invoicing and pgtypes golden
          cases; options, paths, errors); needs uv, git, go, network, and
          docker or POSTGRESQL_SERVER_URI
  verify [--mutants] [--check]
          prove what the generator emits against PostgreSQL (docker or
          SQLC_TEST_DATABASE_URL), on every supported Python, rewriting
          docs/typemap.md and docs/compat.md (docs/python-output.md, Proof);
          with --mutants, also plant bugs that must each be caught; with
          --check, fail instead of writing a document that would change
  sql-text
          (for tests/python/scanner.py) answer each stdin line `[sql, params]`
          with the SQLAlchemy text() of PostgreSQL's `sql`
";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let help = |arg: &&str| matches!(*arg, "help" | "--help" | "-h");
    let result = match args.as_slice() {
        args if args.is_empty() || args.iter().any(help) => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        ["check"] => GenDir::with(check),
        ["bless", rest @ ..] => {
            let allow_regressions = rest.contains(&"--allow-regressions");
            let names: Vec<&str> = (rest.iter().copied())
                .filter(|arg| *arg != "--allow-regressions")
                .collect();
            match names.iter().any(|name| name.starts_with('-')) {
                true => usage(),
                false => GenDir::with(|dir| bless(dir, &names, allow_regressions)),
            }
        }
        // Their scripts parse their own options.
        ["capture", options @ ..] => run(uv_python("tests/python/capture.py").args(options)),
        ["verify", options @ ..] => GenDir::with(|generated| verify(generated, options)),
        ["sql-text"] => sql_text::serve().map_err(|error| format!("{error:#}")),
        _ => usage(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("xtask: {error}");
            ExitCode::FAILURE
        }
    }
}

fn usage() -> Result<(), String> {
    Err(format!("unknown command or option\n\n{USAGE}"))
}

/// Everything that must pass before a change is ready for review. Every step
/// runs, so one report shows everything that is wrong.
fn check(generated: &GenDir) -> Result<(), String> {
    let failed: Vec<String> = [
        run(&mut cargo("fmt --all --check")),
        run(&mut cargo(
            "clippy --workspace --all-targets -- -D warnings",
        )),
        run(&mut generated.test("--workspace")),
        generated.gate(),
        run(&mut uv_python("tests/python/scanner.py")),
    ]
    .into_iter()
    .filter_map(Result::err)
    .collect();
    match failed.is_empty() {
        true => Ok(()),
        false => Err(format!("check failed:\n  {}", failed.join("\n  "))),
    }
}

/// Rewrites the golden cases that changed and the corpus ledger, or only the
/// cases in `names` if there are any.
///
/// Only bless output you have read and verified is more correct than what it
/// replaces. To keep that honest, nothing is written unless the unit specs
/// pass, every corpus output still satisfies the sweep's invariants,
/// everything generated passes the static gate, nothing outside `names` would
/// change, and no corpus request stops generating unless `allow_regressions`
/// says that is intended. The static gate cannot see a wrong type that is
/// still well typed, so a change to a [`PROVEN`] case must also pass the
/// runtime proof.
fn bless(generated: &GenDir, names: &[&str], allow_regressions: bool) -> Result<(), String> {
    if let Some(name) = names.iter().find(|name| case_dir(name).is_none()) {
        return Err(format!(
            "{name} is not a golden case: no examples/*/{name}/"
        ));
    }
    run(&mut generated.test("--quiet --lib"))?;
    // These tests fail on exactly the differences being blessed, so their
    // verdict is not the question here; what they wrote is.
    let _ = run(&mut generated.test("--test rust"));
    let read = |name: &str, failed: &str| {
        fs::read_to_string(generated.0.join(name)).map_err(|_| format!("{failed} (see above)"))
    };
    let changed = read("changed", "the golden test failed")?;
    let changed: Vec<&str> = changed.lines().collect();
    let ledger = read("ledger.tsv", "the corpus sweep failed")?;
    let regressions = read("regressions", "the corpus sweep failed")?;

    let ledger_path = root().join("tests/sqlc/endtoend/ledger.tsv");
    let ledger_changed = fs::read_to_string(&ledger_path).unwrap_or_default() != ledger;
    if !names.is_empty() {
        let mut outside: Vec<&str> = (changed.iter().copied())
            .filter(|name| !names.contains(name))
            .collect();
        if ledger_changed {
            outside.push("the corpus ledger (bless it with no CASE names)");
        }
        if !outside.is_empty() {
            return Err(format!("{} would change too", outside.join(", ")));
        }
    }
    if !regressions.is_empty() && !allow_regressions {
        return Err(format!(
            "these corpus requests would stop generating:\n{regressions}\
             bless with --allow-regressions if that is intended"
        ));
    }
    generated.gate()?;
    if changed.iter().any(|name| PROVEN.contains(name)) {
        run(uv_python("tests/python/verify.py").arg(&generated.0))?;
    }

    println!("\nblessed:");
    for name in &changed {
        let to = case_dir(name).ok_or(format!("{name} is not a golden case"))?;
        let shown = to.strip_prefix(root()).unwrap_or(&to).display().to_string();
        mirror_generated(&generated.0.join(format!("cases__{name}")), &to)
            .map_err(|error| format!("writing {shown}: {error}"))?;
        println!("  {shown}");
    }
    if ledger_changed {
        fs::write(&ledger_path, &ledger).map_err(|error| error.to_string())?;
        println!("  tests/sqlc/endtoend/ledger.tsv");
    }
    if changed.is_empty() && !ledger_changed {
        println!("  nothing: the output already matches");
    }
    Ok(())
}

/// The runtime proof over what the generator emits: `cargo test` exports
/// every golden and corpus output, which `tests/python/verify.py` then proves.
fn verify(generated: &GenDir, options: &[&str]) -> Result<(), String> {
    // Whether the output matches what is committed is `check`'s question: a
    // case awaiting bless is still output to prove. That everything was
    // exported is this one's, and each test writes its file last.
    let _ = run(&mut generated.test("--test rust"));
    if !["changed", "ledger.tsv", "packages.tsv"]
        .iter()
        .all(|file| generated.0.join(file).is_file())
    {
        return Err("cargo test stopped before exporting every output (see above)".into());
    }
    run(uv_python("tests/python/verify.py")
        .arg(&generated.0)
        .args(options))
}

/// The golden cases the runtime proof reads back: the models the type-truth
/// probe reads every type through (`tests/python/typemap.py`), and the acceptance
/// fixtures the runtime suite runs (`tests/python/suite/`).
const PROVEN: [&str; 13] = [
    "billing_pymysql",
    "billing_sqlalchemy",
    "billing_sqlalchemy_async",
    "pgtypes_sqlalchemy",
    "pgtypes_sqlalchemy_async",
    "invoicing_sqlalchemy",
    "invoicing_sqlalchemy_async",
    "invoicing_psycopg",
    "invoicing_psycopg_async",
    "ledger_sqlalchemy",
    "ledger_sqlalchemy_async",
    "ledger_sqlite3",
    "ledger_sqlite3_async",
];

/// The directory of golden case `name`: an example's codegen `out`,
/// `examples/<example>/<name>/`.
fn case_dir(name: &str) -> Option<PathBuf> {
    let examples = fs::read_dir(root().join("examples")).ok()?;
    (examples.filter_map(Result::ok))
        .map(|example| example.path().join(name))
        .find(|dir| dir.is_dir())
}

/// Makes the generated files of `to` exactly those of `from`: all but what
/// tools hide there (`.ruff_cache`).
fn mirror_generated(from: &Path, to: &Path) -> std::io::Result<()> {
    let generated = |dir: &Path| -> std::io::Result<Vec<PathBuf>> {
        let entries = fs::read_dir(dir)?.collect::<std::io::Result<Vec<_>>>()?;
        Ok((entries.into_iter().map(|entry| entry.path()))
            .filter(|path| {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                path.is_file() && !name.starts_with('.')
            })
            .collect())
    };
    for stale in generated(to)? {
        fs::remove_file(stale)?;
    }
    for file in generated(from)? {
        fs::copy(&file, to.join(file.file_name().unwrap()))?;
    }
    Ok(())
}

/// A fresh directory for one invocation's generated output, so concurrent
/// `check` and `bless` runs never gate or bless each other's files.
struct GenDir(PathBuf);

impl GenDir {
    /// Runs `command` with `target/gen/<pid>`, removed if it succeeds. If it
    /// fails, the directory becomes `target/gen/failed`, replacing the last
    /// failure's, for inspection.
    fn with(command: impl FnOnce(&Self) -> Result<(), String>) -> Result<(), String> {
        let parent = root().join("target/gen");
        let generated = Self(parent.join(std::process::id().to_string()));
        let _ = fs::remove_dir_all(&generated.0);
        match command(&generated) {
            Ok(()) => {
                let _ = fs::remove_dir_all(&generated.0);
                Ok(())
            }
            Err(error) if generated.0.is_dir() => {
                let failed = parent.join("failed");
                let _ = fs::remove_dir_all(&failed);
                let _ = fs::rename(&generated.0, &failed);
                Err(format!("{error}\noutput kept in {}", failed.display()))
            }
            Err(error) => Err(error),
        }
    }

    /// `cargo test <args>`, set to write every output here (see
    /// `tests/rust/support/mod.rs`).
    fn test(&self, args: &str) -> Command {
        let mut test = cargo(&format!("test --no-fail-fast {args}"));
        test.env("GEN_DIR", &self.0);
        test
    }

    /// The static Python gate over everything generated; see `tests/python/gate.py`.
    fn gate(&self) -> Result<(), String> {
        run(uv_python("tests/python/gate.py").arg(&self.0))
    }
}

/// `python <arg>` in the `tests/python` uv project, in the Python 3.10
/// environment the static gate also uses, so nothing is created under
/// `tests/python`. `-B -E -s` writes no bytecode and keeps out the caller's
/// `PYTHON*` variables and user site-packages; a script's own directory stays
/// importable.
fn uv_python(arg: &str) -> Command {
    let mut command = Command::new("uv");
    command
        .args(["run", "--quiet", "--project=tests/python", "--locked"])
        .args([
            "--managed-python",
            "--python=3.10",
            "python",
            "-B",
            "-E",
            "-s",
        ])
        .arg(arg)
        .env("UV_PROJECT_ENVIRONMENT", root().join("target/py/3.10"))
        .current_dir(root());
    command
}

/// `cargo <args>` in the workspace root, with the same cargo that runs us.
fn cargo(args: &str) -> Command {
    let mut command = Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    command.args(args.split_whitespace()).current_dir(root());
    command
}

fn run(command: &mut Command) -> Result<(), String> {
    let args: Vec<_> = command
        .get_args()
        .map(|arg| arg.to_string_lossy())
        .collect();
    let program = command.get_program().to_string_lossy();
    let program = Path::new(program.as_ref()).file_name().unwrap_or_default();
    let line = format!("{} {}", program.to_string_lossy(), args.join(" "));
    eprintln!("$ {line}");
    match command.status() {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(format!("`{line}` failed: {status}")),
        Err(error) => Err(format!("`{line}`: {error}")),
    }
}

/// The workspace root, as cargo reports it to this run rather than to the
/// build: a `target/` reused by another copy of the tree (a clone, a CI cache
/// restored elsewhere) would otherwise point every command at the first copy.
fn root() -> PathBuf {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("run by cargo"));
    (manifest.parent())
        .expect("xtask lives one level below the workspace root")
        .to_owned()
}
