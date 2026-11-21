//! We follow the `xtask` convention (a small program in the workspace that `.cargo/config.toml` runs as
//! `cargo xtask`) instead of a Makefile or shell scripts, so there is no build system beyond cargo. The Python side
//! runs through uv.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

mod generate;
mod sql_text;

const USAGE: &str = "\
usage: cargo xtask <command>

commands:
  check   rustfmt --check, clippy with warnings denied, cargo test, the static
          Python gate over every golden case and endtoend output, the scanner
          proof of the SQL rewriting, and ruff and mypy over the scripts of
          tests/python
  bless [--allow-regressions] [--mutants]
          rewrite the golden cases whose output changed (each the codegen `out`
          directory of an examples/*/sqlc.yaml) and the endtoend ledger.
          Refuses when the unit specs, the sweep or the static gate fail, when
          an endtoend request stops generating (unless --allow-regressions), or
          when verify fails on what would change (with --mutants also its
          planted bugs), which also rewrites docs/typemap.md and
          tests/python/exercised.tsv to match (needs docker, or
          SQLC_TEST_DATABASE_URL and SQLC_TEST_MYSQL_URL)
  capture [--sqlc-bin PATH] [--check]
          re-capture tests/sqlc/endtoend and tests/sqlc/examples/*.json with the
          sqlc .prototools pins (downloaded, or the binary at PATH; takes
          several minutes), or with --check fail if that would change anything.
          Then run the WASM plugin under that sqlc and fail unless it keeps the
          wire contract (it writes exactly each example's golden cases;
          options, paths, errors). Needs uv, git, go, network, and docker or
          POSTGRESQL_SERVER_URI
  verify [--mutants] [--check]
          prove what the generator emits against PostgreSQL and MySQL (docker,
          or SQLC_TEST_DATABASE_URL and SQLC_TEST_MYSQL_URL) and SQLite, on the
          oldest and the newest supported Python, and rewrite docs/typemap.md
          and tests/python/exercised.tsv (docs/testing.md). Sweeps again only
          what changed since target/sweep/. With --mutants also plant bugs
          that must each be caught; with --check fail instead of writing a
          document that would change
  generate REQUEST OPTIONS [OUT]
          run the plugin on a request as JSON (tests/sqlc/...) with OPTIONS as
          JSON, printing each file or writing them into OUT
  sql-text
          answer each stdin line `[sql, params]` with the SQLAlchemy text() of
          PostgreSQL's `sql` (for tests/python/scanner.py)
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
        ["bless", flags @ ..] => {
            let known = |flag: &&str| matches!(*flag, "--allow-regressions" | "--mutants");
            match flags.iter().all(known) {
                true => GenDir::with(|generated| bless(generated, flags)),
                false => usage(),
            }
        }
        // Their scripts parse their own options.
        ["capture", options @ ..] => run(uv_python("tests/python/capture.py").args(options)),
        ["verify", options @ ..] => GenDir::with(|generated| verify(generated, options)),
        ["sql-text"] => sql_text::serve().map_err(|error| format!("{error:#}")),
        ["generate", request, options, out @ ..] if out.len() <= 1 => {
            generate::run(request, options, out.first().copied()).map_err(|error| format!("{error:#}"))
        }
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

/// Everything that must pass before a change is ready for review. Every step runs even after one fails, so one report
/// shows everything that is wrong.
fn check(generated: &GenDir) -> Result<(), String> {
    let failed: Vec<String> = [
        run(&mut cargo("fmt --all --check")),
        run(&mut cargo("clippy --workspace --all-targets -- -D warnings")),
        run(&mut generated.test("--workspace")),
        generated.gate(),
        run(&mut uv_python("tests/python/scanner.py")),
        run(uv_python("-m").args(["ruff", "check", "tests/python"])),
        run(uv_python("-m").args(["ruff", "format", "--check", "tests/python"])),
        run(uv_python("-m").args([
            "mypy",
            "--config-file=tests/python/pyproject.toml",
            "--cache-dir=target/py/mypy",
        ])),
    ]
    .into_iter()
    .filter_map(Result::err)
    .collect();
    match failed.is_empty() {
        true => Ok(()),
        false => Err(format!("check failed:\n  {}", failed.join("\n  "))),
    }
}

/// IMPORTANT: Only bless output you have read and verified to be more correct than what it replaces. To keep that
/// honest we write nothing unless the unit specs pass, every endtoend output still holds the sweep's invariants,
/// everything generated passes the static gate and no endtoend request stops generating (unless `--allow-regressions`
/// says that is intended). The static gate cannot see a wrong type that is still well typed, so what changes must also
/// pass the runtime proof, every layer of which reads a golden case or an endtoend output.
fn bless(generated: &GenDir, flags: &[&str]) -> Result<(), String> {
    run(&mut generated.test("--quiet --lib"))?;

    // These tests fail on exactly the differences we are blessing, so we ignore their verdict and read what they wrote.
    let _ = run(&mut generated.test("--test rust"));
    let read = |name: &str, failure: &str| {
        fs::read_to_string(generated.0.join(name)).map_err(|_| format!("{failure} (see above)"))
    };
    let changed = read("changed", "the golden test failed")?;
    let changed: Vec<&str> = changed.lines().collect();
    let ledger = read("ledger.tsv", "the endtoend sweep failed")?;
    let regressions = read("regressions", "the endtoend sweep failed")?;

    let ledger_path = root().join("tests/sqlc/endtoend/ledger.tsv");
    let ledger_changed = fs::read_to_string(&ledger_path).unwrap_or_default() != ledger;

    if !regressions.is_empty() && !flags.contains(&"--allow-regressions") {
        return Err(format!(
            "these endtoend requests would stop generating:\n{regressions}\
             bless with --allow-regressions if that is intended"
        ));
    }

    generated.gate()?;
    if !changed.is_empty() || ledger_changed {
        let mutants = flags.iter().filter(|flag| **flag == "--mutants");
        run(uv_python("tests/python/verify.py").arg(&generated.0).args(mutants))?;
    }

    println!("\nblessed:");
    for case in &changed {
        let shown = format!("examples/{case}");
        let (_, name) = case.split_once('/').expect("golden.rs writes <example>/<case>");
        mirror_generated(&generated.0.join(name), &root().join(&shown))
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

/// The runtime proof over what the generator emits: `cargo test` exports every golden and endtoend output, which
/// `tests/python/verify.py` then proves.
fn verify(generated: &GenDir, options: &[&str]) -> Result<(), String> {
    // We ignore whether the output matches what is committed, as that is `check`'s question and a case awaiting bless
    // is still output to prove. We do need everything exported, and each test writes its file last.
    let _ = run(&mut generated.test("--test rust"));
    if !["changed", "ledger.tsv", "packages.tsv"]
        .iter()
        .all(|file| generated.0.join(file).is_file())
    {
        return Err("cargo test stopped before exporting every output (see above)".into());
    }
    run(uv_python("tests/python/verify.py").arg(&generated.0).args(options))
}

/// Makes the generated files of `to` exactly those of `from`.
fn mirror_generated(from: &Path, to: &Path) -> std::io::Result<()> {
    // NOTE: We skip dot files as tools hide their own there (`.ruff_cache`). Keep this in sync with `generated_files`
    // in tests/rust/support/fixtures.rs.
    let generated = |directory: &Path| -> std::io::Result<Vec<PathBuf>> {
        let entries = fs::read_dir(directory)?.collect::<std::io::Result<Vec<_>>>()?;
        Ok((entries.into_iter().map(|entry| entry.path()))
            .filter(|path| {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                path.is_file() && !name.starts_with('.')
            })
            .collect())
    };

    fs::create_dir_all(to)?;
    for stale in generated(to)? {
        fs::remove_file(stale)?;
    }
    for file in generated(from)? {
        fs::copy(&file, to.join(file.file_name().unwrap()))?;
    }
    Ok(())
}

/// A fresh directory for one invocation's generated output, so concurrent `check` and `bless` runs never gate or bless
/// each other's files.
struct GenDir(PathBuf);

impl GenDir {
    /// Runs `command` with `target/gen/<pid>` and removes it on success. On failure we keep it for inspection as
    /// `target/gen/failed`, replacing the previous failure's.
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

    /// `cargo test <args>`, set to write every output here (see `tests/rust/support/`).
    fn test(&self, args: &str) -> Command {
        let mut test = cargo(&format!("test --no-fail-fast {args}"));
        test.env("GEN_DIR", &self.0);
        test
    }

    fn gate(&self) -> Result<(), String> {
        run(uv_python("tests/python/gate.py").arg(&self.0))
    }
}

/// The oldest supported Python, as the `tests/python` project's `requires-python` says: `>=3.11` is `3.11`.
fn minimum_python() -> String {
    let path = root().join("tests/python/pyproject.toml");
    let project: toml::Table = toml::from_str(&fs::read_to_string(&path).expect("tests/python/pyproject.toml"))
        .expect("tests/python/pyproject.toml is TOML");
    let python = project["project"]["requires-python"]
        .as_str()
        .and_then(|requires| requires.strip_prefix(">="));
    python
        .expect("tests/python/pyproject.toml says `requires-python = \">=3.x\"`")
        .to_string()
}

/// `python <arg>` in the `tests/python` uv project, in the environment of the oldest supported Python, which the static
/// gate also uses. We keep
/// everything under `target/`, what Hypothesis keeps of its draws included, so nothing is created under `tests/python`
/// or anywhere else in the tree.
fn uv_python(arg: &str) -> Command {
    // Notes:
    // - `-B` writes no bytecode
    // - `-E` and `-s` keep out the caller's `PYTHON*` variables and user site-packages, while a script's own directory
    //   stays importable (which `-I` would not allow)
    let python = minimum_python();
    let mut command = Command::new("uv");
    command
        .args([
            "run",
            "--quiet",
            "--project=tests/python",
            "--locked",
            "--managed-python",
        ])
        .arg(format!("--python={python}"))
        .args(["python", "-B", "-E", "-s"])
        .arg(arg)
        .env("UV_PROJECT_ENVIRONMENT", root().join("target/py").join(&python))
        .env("HYPOTHESIS_STORAGE_DIRECTORY", root().join("target/py/hypothesis"))
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
    let args: Vec<_> = command.get_args().map(|arg| arg.to_string_lossy()).collect();
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

/// We ask cargo for the workspace root of this run rather than of the build, as a `target/` reused by another copy of
/// the tree (a clone, a CI cache restored elsewhere) would otherwise point every command at the first copy.
fn root() -> PathBuf {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("run by cargo"));
    (manifest.parent())
        .expect("xtask lives one level below the workspace root")
        .to_owned()
}
