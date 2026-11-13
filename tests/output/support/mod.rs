//! What the golden tests and the corpus sweep share: reading fixtures, the
//! invariants every generated output must satisfy, and handing output to
//! `cargo xtask` for the static gate.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

use fancy_regex::Regex;

use ruff_python_ast::token::TokenKind;
use ruff_python_ast::visitor::{Visitor, walk_expr, walk_stmt};
use ruff_python_ast::{
    Expr, ExprCall, ExprDict, ModModule, Operator, Stmt, StmtClassDef, StmtFunctionDef,
};
use ruff_python_formatter::{PyFormatOptions, format_module_source};
use ruff_python_parser::{Parsed, parse_module};
use ruff_python_stdlib::builtins::is_python_builtin;
use ruff_text_size::{Ranged, TextRange};
use sqlc_gen_python::api::models::{self, Repeats};
use sqlc_gen_python::backend::Backend;
use sqlc_gen_python::config::Config;
use sqlc_gen_python::engine::{Engine, Piece};
use sqlc_gen_python::plugin;

/// The source of an expression, as ruff writes it.
fn unparse(expr: &Expr) -> String {
    ruff_python_codegen::Generator::new(
        &ruff_python_codegen::Indentation::default(),
        ruff_source_file::LineEnding::Lf,
    )
    .expr(expr)
}

/// The repository root, as cargo reports it to this run rather than to the
/// build: a `target/` reused by another copy of the tree would otherwise test
/// the first copy's fixtures.
pub fn root() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("run by cargo"))
}

/// Reads a JSON fixture, panicking with the path if it is missing or invalid.
pub fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> T {
    let contents = fs::read_to_string(path).unwrap_or_else(|error| {
        panic!("reading {path:?}: {error} (fixtures come from `cargo xtask capture`)")
    });
    serde_json::from_str(&contents).unwrap_or_else(|error| panic!("parsing {path:?}: {error}"))
}

/// What a golden case under `tests/cases/` generates, and from what: its
/// `case.json`.
#[derive(Debug, serde::Deserialize)]
struct Case {
    /// The schema fixture under `tests/schemas/`, without the extension.
    schema: String,
    /// The plugin options, exactly as they would appear under `options:`.
    #[serde(default)]
    options: serde_json::Map<String, serde_json::Value>,
}

/// The request a golden case directory generates from: its schema fixture,
/// with its options.
pub fn case_request(case_dir: &Path) -> plugin::GenerateRequest {
    let case: Case = read_json(&case_dir.join("case.json"));
    let fixture = root().join(format!("tests/schemas/{}.json", case.schema));
    let mut request: plugin::GenerateRequest = read_json(&fixture);
    request.plugin_options = serde_json::to_vec(&case.options).unwrap();
    request
}

/// The files a golden case holds as generated, as a plugin returns them, by
/// name: all but its `case.json`, and what tools hide there (`.ruff_cache`).
pub fn generated_files(dir: &Path) -> Vec<plugin::File> {
    let mut files: Vec<plugin::File> = (fs::read_dir(dir).unwrap())
        .map(|entry| entry.unwrap().path())
        .filter(|path| is_generated(path))
        .map(|path| plugin::File {
            name: path.file_name().unwrap().to_string_lossy().into_owned(),
            contents: fs::read(&path).unwrap(),
        })
        .collect();
    files.sort_by(|a, b| a.name.cmp(&b.name));
    files
}

/// Whether a file of a golden case is one the plugin generated.
fn is_generated(path: &Path) -> bool {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    path.is_file() && name != "case.json" && !name.starts_with('.')
}

/// `GEN_DIR`, where `cargo xtask` collects what the tests generate.
pub fn gen_dir() -> Option<PathBuf> {
    std::env::var_os("GEN_DIR").map(PathBuf::from)
}

/// Writes one generated output as the package `<GEN_DIR>/<package>/`, where
/// `cargo xtask check` runs the Python toolchain over it. Does nothing unless
/// `GEN_DIR` is set.
pub fn export(package: &str, files: &[plugin::File]) {
    let Some(dir) = gen_dir() else { return };
    for file in files {
        let path = dir.join(package).join(&file.name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &file.contents).unwrap();
    }
}

/// Every invariant of what `request` generated, as listed in `corpus.rs`;
/// empty if it holds them all.
pub fn check_output(request: &plugin::GenerateRequest, files: &[plugin::File]) -> Vec<String> {
    let mut problems = Vec::new();
    let stems: BTreeSet<&str> = (request.queries.iter())
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
    let body = |file: &str| -> Option<&[Stmt]> {
        (modules.iter())
            .find(|(name, ..)| *name == file)
            .map(|(_, _, parsed)| &parsed.syntax().body[..])
    };
    let models = body("models.py").unwrap_or_default();
    for (name, source, parsed) in &modules {
        let found = module_problems(parsed, source);
        problems.extend(found.into_iter().map(|p| format!("{name}: {p}")));
    }
    for stem in stems {
        let file = format!("{stem}.py");
        if let Some(module) = body(&file) {
            let mut found = Vec::new();
            queries_follow_sqlc(request, stem, module, models, &mut found);
            problems.extend(found.into_iter().map(|p| format!("{file}: {p}")));
        }
    }
    models_follow_catalog(request, models, &mut problems);
    problems
}

/// Whether a generated file name stays inside the output directory.
fn is_inside_out(name: &str) -> bool {
    let path = Path::new(name);
    path.components().next().is_some()
        && path.components().all(|c| matches!(c, Component::Normal(_)))
}

/// Every invariant of one generated module's source; empty if it holds them
/// all.
pub fn check_module(source: &str) -> Vec<String> {
    match parse_module(source) {
        Ok(parsed) => module_problems(&parsed, source),
        Err(error) => vec![format!("does not parse: {error}")],
    }
}

/// Every invariant of one parsed module, which each check sees as a tree as
/// well as text.
fn module_problems(parsed: &Parsed<ModModule>, source: &str) -> Vec<String> {
    let module = parsed.syntax();
    let mut problems = Vec::new();
    match format_module_source(source, PyFormatOptions::default()) {
        Ok(formatted) if formatted.as_code() == source => {}
        Ok(_) => problems.push("is not a fixpoint of ruff's formatter".into()),
        Err(error) => problems.push(format!("does not format: {error}")),
    }
    comments_are_the_header(parsed, source, &mut problems);
    scopes("module", &module.body, &mut problems);
    reads_resolve(&module.body, &mut problems);
    annotations_are_precise(&module.body, &mut problems);
    records_are_values(&module.body, &mut problems);
    problems
}

/// Checks that the only comments are the lines the module opens with, the
/// header (§1, rule 3). Any other comment says something to a tool: a
/// `# type: ignore` silences both type checkers on its line, which the static
/// gate, deaf only to `# noqa`, would then pass.
fn comments_are_the_header(parsed: &Parsed<ModModule>, source: &str, problems: &mut Vec<String>) {
    let mut code = false;
    for token in parsed.tokens().iter() {
        match token.kind() {
            TokenKind::Comment if code => {
                problems.push(format!(
                    "`{}` is a comment after the header",
                    &source[token.range()]
                ));
            }
            TokenKind::Comment | TokenKind::NonLogicalNewline | TokenKind::Newline => {}
            _ => code = true,
        }
    }
}

/// Checks that every class but an enum and an error is a record (§4, rule
/// 2): exactly the one dataclass decorator that makes it frozen, slotted and
/// keyword-only, and no field with a default, so an omitted value is a type
/// error, never a NULL.
fn records_are_values(body: &[Stmt], problems: &mut Vec<String>) {
    for class in body.iter().filter_map(Stmt::as_class_def_stmt) {
        if is_enum(class) || class.bases().iter().map(unparse).eq(["LookupError"]) {
            continue;
        }
        let record = "dataclasses.dataclass(frozen=True, slots=True, kw_only=True)";
        let bases = class.bases();
        let decorators: Vec<String> = (class.decorator_list.iter())
            .map(|decorator| unparse(&decorator.expression))
            .collect();
        if decorators != [record] || !bases.is_empty() {
            problems.push(format!("`{}` is not exactly a `@{record}`", class.name));
        }
        for field in (class.body.iter().filter_map(Stmt::as_ann_assign_stmt))
            .filter(|field| field.value.is_some())
        {
            problems.push(format!(
                "`{}.{}` has a default",
                class.name,
                unparse(&field.target)
            ));
        }
    }
}

/// Checks the names each scope binds.
///
/// The parser has already proven every name a syntactically valid identifier,
/// keywords included. What it accepts and Python then gets wrong is checked
/// here: a name bound twice in one scope silently loses the first (a field, an
/// enum member, a class shadowing an import), and a leading underscore makes a
/// name private — a double underscore is mangled, and `_sunder_` names are
/// reserved by `enum`. Dunder methods (`__str__`) are neither, and neither is
/// a module's `_UPPER_SNAKE` constant, such as the statement a query function
/// runs: an implementation detail of the module, which the underscore keeps
/// out of its public interface (`from m import *`, completion) and out of the
/// way of the function named after the same query.
fn scopes(scope: &str, body: &[Stmt], problems: &mut Vec<String>) {
    bound(scope, body.iter().flat_map(bindings), problems);
    for stmt in body {
        match stmt {
            Stmt::ClassDef(class) => {
                scopes(&format!("class {}", class.name), &class.body, problems)
            }
            Stmt::FunctionDef(function) => bound(
                &format!("def {}", function.name),
                function.parameters.iter().map(|p| p.name().as_str()),
                problems,
            ),
            _ => {}
        }
    }
}

fn bound<'a>(scope: &str, names: impl Iterator<Item = &'a str>, problems: &mut Vec<String>) {
    let mut seen = BTreeSet::new();
    for name in names {
        if !seen.insert(name) {
            problems.push(format!("{scope}: `{name}` is bound twice"));
        }
        let dunder = name.len() > 4 && name.starts_with("__") && name.ends_with("__");
        let constant = scope == "module"
            && (name.strip_prefix('_')).is_some_and(|rest| {
                rest.starts_with(|c: char| c.is_ascii_uppercase())
                    && rest
                        .chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
            });
        if name.starts_with('_') && !dunder && !constant {
            problems.push(format!("{scope}: `{name}` is private"));
        }
    }
}

/// Checks that every name the module reads is bound when it is read: by an
/// earlier statement, or as a builtin of Python 3.10, the oldest supported.
///
/// Class bodies and their annotations run on import, so a name read too early
/// or never bound — a model annotated with a class that is not generated — is
/// a `NameError` the moment anyone imports the module.
fn reads_resolve(body: &[Stmt], problems: &mut Vec<String>) {
    let mut bound = BTreeSet::new();
    for stmt in body {
        let mut reads = Names::default();
        reads.visit_stmt(stmt);
        for name in reads.0 {
            if !bound.contains(name) && !is_python_builtin(name, 10, false) {
                problems.push(format!("`{name}` is read before it is bound"));
            }
        }
        bound.extend(bindings(stmt));
    }
}

/// Checks that no annotation mentions `Any`, but those that are the library's
/// own: the row `from_row` reads (`row: Sequence[Any]`), what `:execresult`
/// returns (`sqlalchemy.CursorResult[Any]`, a psycopg cursor of tuples), and
/// a psycopg connection, whatever rows it makes.
///
/// `Any` passes every type checker, so it is the one annotation the static
/// gate cannot catch being wrong; the generator must say what a column or
/// parameter is, or fail (`docs/python-output.md` §5).
fn annotations_are_precise(body: &[Stmt], problems: &mut Vec<String>) {
    /// Each annotation, described, and whether it is the driver's own.
    #[derive(Default)]
    struct Annotations<'a>(Vec<(String, &'a Expr, &'static [&'static str])>);
    impl<'a> Visitor<'a> for Annotations<'a> {
        fn visit_stmt(&mut self, stmt: &'a Stmt) {
            match stmt {
                Stmt::AnnAssign(field) => {
                    let what = format!("field `{}`", unparse(&field.target));
                    self.0.push((what, &field.annotation, &[]));
                }
                Stmt::FunctionDef(function) => {
                    let name = &function.name;
                    for parameter in function.parameters.iter() {
                        if let Some(annotation) = parameter.annotation() {
                            let own = match (name.as_str(), parameter.name().as_str()) {
                                ("from_row", "row") => &["Sequence[Any]"][..],
                                (_, "conn") => {
                                    &["psycopg.Connection[Any]", "psycopg.AsyncConnection[Any]"]
                                }
                                _ => &[],
                            };
                            let what = format!("parameter `{}` of `{name}`", parameter.name());
                            self.0.push((what, annotation, own));
                        }
                    }
                    if let Some(returns) = &function.returns {
                        let what = format!("the result of `{name}`");
                        let own = &[
                            "sqlalchemy.CursorResult[Any]",
                            "psycopg.RawCursor[tuple[Any, ...]]",
                            "psycopg.AsyncRawCursor[tuple[Any, ...]]",
                        ];
                        self.0.push((what, returns, own));
                    }
                }
                _ => {}
            }
            walk_stmt(self, stmt);
        }
    }

    /// Whether an expression mentions `Any`, as `Any` or `typing.Any`.
    #[derive(Default)]
    struct MentionsAny(bool);
    impl<'a> Visitor<'a> for MentionsAny {
        fn visit_expr(&mut self, expr: &'a Expr) {
            self.0 |= match expr {
                Expr::Name(name) => name.id.as_str() == "Any",
                Expr::Attribute(attribute) => attribute.attr.as_str() == "Any",
                _ => false,
            };
            walk_expr(self, expr);
        }
    }

    let mut annotations = Annotations::default();
    annotations.visit_body(body);
    for (what, annotation, own) in annotations.0 {
        let mut any = MentionsAny::default();
        any.visit_expr(annotation);
        if any.0 && !own.contains(&unparse(annotation).as_str()) {
            problems.push(format!("{what} is annotated with `Any`"));
        }
    }
}

/// The names an expression or statement reads on import, in order: a
/// function's signature runs when it is defined, its body only when called.
#[derive(Default)]
struct Names<'a>(Vec<&'a str>);

impl<'a> Visitor<'a> for Names<'a> {
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        match stmt {
            Stmt::FunctionDef(function) => {
                for decorator in &function.decorator_list {
                    self.visit_decorator(decorator);
                }
                self.visit_parameters(&function.parameters);
                if let Some(returns) = &function.returns {
                    self.visit_annotation(returns);
                }
            }
            _ => walk_stmt(self, stmt),
        }
    }

    fn visit_expr(&mut self, expr: &'a Expr) {
        if let Expr::Name(name) = expr
            && name.ctx.is_load()
        {
            self.0.push(name.id.as_str());
        }
        walk_expr(self, expr);
    }
}

/// The names one statement binds in the scope it appears in.
fn bindings(stmt: &Stmt) -> Vec<&str> {
    fn target(expr: &Expr) -> Option<&str> {
        expr.as_name_expr().map(|name| name.id.as_str())
    }
    match stmt {
        Stmt::Import(import) => (import.names.iter())
            .map(|alias| match &alias.asname {
                Some(asname) => asname.as_str(),
                None => alias.name.split('.').next().unwrap_or_default(),
            })
            .collect(),
        Stmt::ImportFrom(import) => (import.names.iter())
            .map(|alias| alias.asname.as_ref().unwrap_or(&alias.name).as_str())
            .collect(),
        Stmt::ClassDef(class) => vec![class.name.as_str()],
        Stmt::FunctionDef(function) => vec![function.name.as_str()],
        Stmt::Assign(assign) => assign.targets.iter().filter_map(target).collect(),
        Stmt::AnnAssign(assign) => target(&assign.target).into_iter().collect(),
        _ => Vec::new(),
    }
}

/// Checks that the module of query file `stem` implements sqlc's queries of
/// it (`docs/python-output.md` §6).
///
/// The module holds one statement constant and one function per query, in
/// sqlc's order, so they pair up without repeating any naming rule. Each pair
/// must run exactly the SQL sqlc wrote, and its signature must say what sqlc
/// says of NULL and arrays: a parameter, result field or bare result
/// annotated optional exactly when sqlc says it admits NULL, and a list
/// exactly as deep as its array. No type checker or runtime probe can see a
/// non-null value annotated `X | None`.
fn queries_follow_sqlc(
    request: &plugin::GenerateRequest,
    stem: &str,
    body: &[Stmt],
    models: &[Stmt],
    problems: &mut Vec<String>,
) {
    let Ok(config) = Config::from_request(request) else {
        return problems.push("generated from options that do not parse".into());
    };
    let (backend, asynchronous) = (config.backend, config.asynchronous);
    let Ok(engine) = Engine::of(request) else {
        return problems.push("generated for an engine it does not support".into());
    };
    let queries: Vec<&plugin::Query> = (request.queries.iter())
        .filter(|query| self::stem(&query.filename) == stem)
        .collect();
    let statements: Vec<(&str, String)> = (body.iter())
        .filter_map(|stmt| {
            let constant = stmt.as_ann_assign_stmt()?;
            let sql = statement_sql(constant.value.as_deref()?, backend)?;
            is_name(&constant.annotation, "Final")
                .then_some((constant.target.as_name_expr()?.id.as_str(), sql))
        })
        .collect();
    let functions = functions(body);
    if statements.len() != queries.len() || functions.len() != queries.len() {
        return problems.push(format!(
            "{} statements and {} functions for {} queries",
            statements.len(),
            functions.len(),
            queries.len()
        ));
    }
    let conn = backend.connection(asynchronous);
    for ((query, (constant, sql)), function) in queries.into_iter().zip(statements).zip(functions) {
        let parameters = parameters(body, function);
        if function_follows_sqlc(query, function, &parameters, conn, asynchronous, problems) {
            runs_its_statement(
                query,
                function,
                constant,
                &parameters,
                asynchronous,
                problems,
            );
            result_follows_sqlc(request, query, function, body, models, backend, problems);
            sql_round_trips(query, &sql, &parameters, backend, engine, problems);
        }
    }
}

/// A module's functions, in order.
fn functions(body: &[Stmt]) -> Vec<&StmtFunctionDef> {
    body.iter().filter_map(Stmt::as_function_def_stmt).collect()
}

/// Checks one query's function's signature and documentation against sqlc's
/// query; whether its parameters pair up with sqlc's, for the checks after.
fn function_follows_sqlc(
    query: &plugin::Query,
    function: &StmtFunctionDef,
    parameters: &[(&str, &Expr)],
    conn: &str,
    asynchronous: bool,
    problems: &mut Vec<String>,
) -> bool {
    signature_is_safe(query, function, conn, asynchronous, problems);
    if parameters.len() != query.params.len() {
        let names: Vec<&str> = parameters.iter().map(|(name, _)| *name).collect();
        problems.push(format!(
            "{}: parameters {names:?} for {} placeholders",
            query.name,
            query.params.len()
        ));
        return false;
    }
    let spellings: Vec<&str> = (query.params.iter())
        .map(|param| param.column.as_ref().map_or("", |c| c.name.as_str()))
        .collect();
    let what = format!("{}: parameters", query.name);
    named_after(&what, parameters, &spellings, Repeats::Numbered, problems);
    if words(&docstring(function.body.first())) != words(&query.comments.join("\n")) {
        problems.push(format!("{}: its docstring is not its comment", query.name));
    }
    for ((name, annotation), param) in parameters.iter().zip(&query.params) {
        let column = param.column.clone().unwrap_or_default();
        if Shape::of(annotation) != Shape::from(&column) {
            problems.push(format!(
                "{}: parameter `{name}` disagrees with sqlc's ${} on NULL or array depth",
                query.name, param.number
            ));
        }
    }
    true
}

/// A query file's stem, which names its module.
fn stem(filename: &str) -> &str {
    Path::new(filename)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default()
}

/// Checks what makes one query's function safe to call (§6, "Functions"):
/// `conn` first and positional, annotated as the backend's connection, then
/// only the `rows` of a `:copyfrom`, every other parameter keyword-only; no
/// defaults, so none can be forgotten; and `async def` exactly when the
/// options ask for coroutines.
fn signature_is_safe(
    query: &plugin::Query,
    function: &StmtFunctionDef,
    conn: &str,
    asynchronous: bool,
    problems: &mut Vec<String>,
) {
    let parameters = &function.parameters;
    let positional: Vec<&str> = (parameters.posonlyargs.iter().chain(&parameters.args))
        .map(|p| p.name().as_str())
        .collect();
    let copyfrom = query.cmd == ":copyfrom";
    let expected: &[&str] = if copyfrom {
        &["conn", "rows"]
    } else {
        &["conn"]
    };
    if positional != expected
        || !parameters.posonlyargs.is_empty()
        || parameters.vararg.is_some()
        || parameters.kwarg.is_some()
        || (copyfrom && !parameters.kwonlyargs.is_empty())
    {
        problems.push(format!(
            "{}: takes {positional:?} positionally, not {expected:?} and the rest keyword-only",
            query.name
        ));
    }
    if parameters
        .iter_non_variadic_params()
        .any(|p| p.default.is_some())
    {
        problems.push(format!("{}: a parameter has a default", query.name));
    }
    let annotation = (parameters.args.first())
        .and_then(|conn| conn.annotation())
        .map(unparse);
    if annotation.as_deref() != Some(conn) {
        problems.push(format!(
            "{}: takes a connection {annotation:?}, not `{conn}`",
            query.name
        ));
    }
    if function.is_async != asynchronous {
        problems.push(format!("{}: is `async def` exactly when async", query.name));
    }
}

/// Checks that one query's function executes its own statement, exactly once
/// and awaited exactly when async, binding each parameter as itself: under its
/// own name (`{"p": p}`) on SQLAlchemy, or in its own place in a tuple
/// (`(p, q)`), wrapped at most in a JSON type, on a driver; for each row of a
/// `:copyfrom`, as `r.p`. A function run on its neighbour's statement, or two
/// values of one type swapped, would type-check and run. And that this is
/// all it does with the connection: the caller owns the transaction, which a
/// `conn.commit()` would end.
fn runs_its_statement(
    query: &plugin::Query,
    function: &StmtFunctionDef,
    constant: &str,
    parameters: &[(&str, &Expr)],
    asynchronous: bool,
    problems: &mut Vec<String>,
) {
    #[derive(Default)]
    struct Executes<'a> {
        calls: Vec<&'a ExprCall>,
        conns: usize,
        awaited: Vec<TextRange>,
    }
    impl<'a> Visitor<'a> for Executes<'a> {
        fn visit_expr(&mut self, expr: &'a Expr) {
            match expr {
                Expr::Call(call)
                    if call.func.as_attribute_expr().is_some_and(|method| {
                        ["execute", "executemany"].contains(&method.attr.as_str())
                    }) =>
                {
                    self.calls.push(call)
                }
                Expr::Name(name) if name.id.as_str() == "conn" => self.conns += 1,
                Expr::Await(awaited) => self.awaited.push(awaited.value.range()),
                _ => {}
            }
            walk_expr(self, expr);
        }
    }
    let mut executes = Executes::default();
    executes.visit_body(&function.body);
    let ([call], 1) = (&executes.calls[..], executes.conns) else {
        return problems.push(format!(
            "{}: executes {} statements, and reads `conn` {} times, not only to execute one",
            query.name,
            executes.calls.len(),
            executes.conns
        ));
    };
    if executes.awaited.contains(&call.range()) != asynchronous {
        problems.push(format!(
            "{}: awaits its statement exactly when async",
            query.name
        ));
    }
    let args = &call.arguments.args;
    if !args.first().is_some_and(|first| is_name(first, constant))
        || !call.arguments.keywords.is_empty()
    {
        let args: Vec<String> = args.iter().map(unparse).collect();
        return problems.push(format!(
            "{}: executes ({}), not its own statement `{constant}`",
            query.name,
            args.join(", ")
        ));
    }

    let copyfrom = query.cmd == ":copyfrom";
    let value = |name: &str| match copyfrom {
        true => format!("r.{name}"),
        false => name.to_string(),
    };
    // Each parameter as it is bound: by name, or by its number's position.
    let mut expected: Vec<(String, String)> = (parameters.iter().zip(&query.params))
        .map(|((name, _), param)| (format!("${}", param.number), value(name)))
        .collect();
    let bound = match (&args[1..], copyfrom) {
        ([], false) => Some(Vec::new()),
        ([Expr::Dict(dict)], false) => Some(pairs(dict)),
        ([Expr::Tuple(tuple)], false) => Some(positions(&tuple.elts)),
        // SQLAlchemy's `executemany` form: a list of dicts built before.
        ([rows], true) if is_name(rows, "params") => {
            let mut dicts = Vec::new();
            for stmt in &function.body {
                if let Some(Expr::ListComp(list)) = stmt.as_assign_stmt().map(|a| &*a.value)
                    && let Expr::Dict(dict) = &*list.elt
                {
                    dicts.push(pairs(dict));
                }
            }
            (dicts.len() == 1).then(|| dicts.remove(0))
        }
        ([Expr::ListComp(list)], true) => match &*list.elt {
            Expr::Tuple(tuple) => Some(positions(&tuple.elts)),
            _ => None,
        },
        _ => None,
    };
    if bound
        .as_ref()
        .is_some_and(|bound| bound.iter().all(|(key, _)| !key.starts_with('$')))
    {
        expected = (parameters.iter())
            .map(|(name, _)| (name.to_string(), value(name)))
            .collect();
    } else {
        expected.sort_by_key(|(number, _)| number[1..].parse::<i32>().unwrap_or_default());
    }
    if bound.as_ref() != Some(&expected) {
        problems.push(format!(
            "{}: binds {bound:?}, not each parameter as itself {expected:?}",
            query.name
        ));
    }
}

/// A dict's items, each its key and value.
fn pairs(dict: &ExprDict) -> Vec<(String, String)> {
    (dict.items.iter())
        .map(|item| {
            let key = item.key.as_ref().and_then(Expr::as_string_literal_expr);
            let key = key.map_or("?".into(), |key| key.value.to_str().into());
            (key, unparse(&item.value))
        })
        .collect()
}

/// A tuple's items, each as the placeholder `$N` of its position and its
/// value, unwrapped from a JSON type: `None if v is None else Jsonb(v)` is
/// `v`.
fn positions(items: &[Expr]) -> Vec<(String, String)> {
    let unwrapped = |item: &Expr| {
        let Expr::If(choice) = item else {
            return unparse(item);
        };
        let wrapped = (choice.orelse.as_call_expr())
            .filter(|call| {
                ["Json", "Jsonb"]
                    .iter()
                    .any(|json| is_name(&call.func, json))
            })
            .and_then(|call| call.arguments.args.first());
        match wrapped {
            Some(value)
                if choice.body.is_none_literal_expr()
                    && unparse(&choice.test) == format!("{} is None", unparse(value)) =>
            {
                unparse(value)
            }
            _ => unparse(item),
        }
    };
    (items.iter().enumerate())
        .map(|(i, item)| (format!("${}", i + 1), unwrapped(item)))
        .collect()
}

/// Checks that a record's `from_row` builds its fields, in order, each from
/// its own place in the row: a column from `row[i]` (inside an enum's
/// conversion, if any), and an embed of `width` columns from `row[a:b]`,
/// starting where the field before it ended. `widths` gives each field's:
/// `None` for a column.
fn reads_in_order(class: &StmtClassDef, widths: &[Option<usize>], problems: &mut Vec<String>) {
    #[derive(Default)]
    struct Reads(BTreeSet<String>);
    impl<'a> Visitor<'a> for Reads {
        fn visit_expr(&mut self, expr: &'a Expr) {
            if let Expr::Subscript(subscript) = expr
                && is_name(&subscript.value, "row")
            {
                self.0.insert(unparse(&subscript.slice));
            }
            walk_expr(self, expr);
        }
    }

    let built = (class.body.iter().filter_map(Stmt::as_function_def_stmt))
        .find(|method| method.name.as_str() == "from_row")
        .and_then(|method| method.body.last()?.as_return_stmt()?.value.as_deref())
        .and_then(Expr::as_call_expr);
    let names: Vec<&str> = fields(class).into_iter().map(|(name, _)| name).collect();
    let Some(built) = built else {
        // A table without columns has no row to read.
        if !names.is_empty() {
            problems.push(format!("class `{}`: no `from_row` to build it", class.name));
        }
        return;
    };
    let keywords: Vec<&str> = (built.arguments.keywords.iter())
        .filter_map(|keyword| Some(keyword.arg.as_ref()?.as_str()))
        .collect();
    if keywords != names || !built.arguments.args.is_empty() || widths.len() != names.len() {
        return problems.push(format!(
            "class `{}`: `from_row` builds {keywords:?}, not its fields {names:?} by keyword",
            class.name
        ));
    }
    let mut start = 0;
    for (keyword, width) in built.arguments.keywords.iter().zip(widths) {
        let expected = match width {
            None => start.to_string(),
            Some(width) => format!("{start}:{}", start + width),
        };
        start += width.unwrap_or(1);
        let mut reads = Reads::default();
        reads.visit_expr(&keyword.value);
        if reads.0 != BTreeSet::from([expected.clone()]) {
            problems.push(format!(
                "class `{}`: `from_row` reads row[{}] into `{}`, not row[{expected}]",
                class.name,
                reads.0.into_iter().collect::<Vec<_>>().join("], row["),
                keyword.arg.as_ref().map_or("?", |arg| arg.as_str()),
            ));
        }
    }
}

/// The catalog table an identifier names, the default schema if it names
/// none.
fn table<'a>(
    request: &'a plugin::GenerateRequest,
    id: &plugin::Identifier,
) -> Option<&'a plugin::Table> {
    let catalog = request.catalog.as_ref()?;
    let schema = match id.schema.as_str() {
        "" => catalog.default_schema.as_str(),
        schema => schema,
    };
    (catalog.schemas.iter().filter(|s| s.name == schema))
        .flat_map(|s| &s.tables)
        .find(|t| t.rel.as_ref().is_some_and(|rel| rel.name == id.name))
}

/// Whether a result is exactly one table's columns, so that its rows are that
/// table's model, as sqlc-gen-go reuses one: at least two columns, each of
/// that table, with its columns' names, order, types, nullability and array
/// depth.
fn reuses_model(request: &plugin::GenerateRequest, columns: &[plugin::Column]) -> bool {
    let of = |column: &plugin::Column| column.table.as_ref().and_then(|t| table(request, t));
    let Some(model) = columns.first().and_then(of) else {
        return false;
    };
    let same = |a: &plugin::Column, b: &plugin::Column| {
        (a.name == b.name && a.data_type() == b.data_type() && a.not_null == b.not_null)
            && Shape::from(a) == Shape::from(b)
    };
    columns.len() >= 2
        && columns.len() == model.columns.len()
        && columns
            .iter()
            .all(|c| of(c).is_some_and(|t| std::ptr::eq(t, model)))
        && columns.iter().zip(&model.columns).all(|(a, b)| same(a, b))
}

/// Checks that one query's statement runs exactly the SQL sqlc wrote, given
/// the names it binds its parameters under, in sqlc's order. A driver is
/// given sqlc's text itself; SQLAlchemy's goes through what SQLAlchemy does
/// with it, with its own expressions copied verbatim: find the binds, which
/// must be exactly the function's parameters; render each as its `$N`; and
/// unescape `\:`. The result must be sqlc's `query.text`, byte for byte.
fn sql_round_trips(
    query: &plugin::Query,
    sql: &str,
    parameters: &[(&str, &Expr)],
    backend: Backend,
    engine: Engine,
    problems: &mut Vec<String>,
) {
    if backend != Backend::SqlAlchemy {
        if sql != query.text {
            problems.push(format!(
                "{}: runs {sql:?}, not sqlc's {:?}",
                query.name, query.text
            ));
        }
        return;
    }
    // SQLAlchemy's `TextClause._bind_params_regex`, then its
    // `compiler.BIND_PARAMS_ESC`, verbatim: the runtime suite checks the copy
    // against the SQLAlchemy it runs (`test_contract.py`). Each `\w` matches
    // as Python's does, a letter, a number or `_` (which `test_contract.py`
    // checks over every code point), not as this engine's, which also takes
    // combining marks and connector punctuation.
    static PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
        (include_str!("sqlalchemy.re").lines())
            .map(|pattern| Regex::new(&pattern.replace(r"\w", r"[\p{L}\p{N}_]")).unwrap())
            .collect()
    });
    let [bind, escaped] = &PATTERNS[..] else {
        unreachable!("sqlalchemy.re holds two patterns")
    };

    let names: Vec<&str> = parameters.iter().map(|(name, _)| *name).collect();
    let binds: BTreeSet<&str> = (bind.captures_iter(sql))
        .map(|captures| captures.unwrap().get(1).unwrap().as_str())
        .collect();
    if binds != names.iter().copied().collect() {
        problems.push(format!(
            "{}: binds {binds:?} are not its parameters {names:?}",
            query.name
        ));
        return;
    }
    // Each placeholder of sqlc's text, as written, and the parameter it binds.
    let numbered: Vec<(i32, &str)> = (query.params.iter().map(|p| p.number))
        .zip(names.iter().copied())
        .collect();
    let Ok(pieces) = engine.sql(&query.text, &numbered) else {
        return problems.push(format!("{}: sqlc's text does not cut", query.name));
    };
    let mut placeholders = (pieces.0.into_iter()).filter_map(|piece| match piece {
        Piece::Param { placeholder, name } => Some((placeholder, name)),
        Piece::Text(_) => None,
    });
    let mut misplaced = Vec::new();
    let rendered = bind.replace_all(sql, |captures: &fancy_regex::Captures<'_, str>| {
        let (placeholder, name) = placeholders.next().unwrap_or_default();
        if name != captures[1] {
            misplaced.push(format!("{} at {placeholder}", &captures[1]));
        }
        placeholder
    });
    let rendered = escaped.replace_all(&rendered, "$1");
    if !misplaced.is_empty() || rendered != query.text {
        problems.push(format!(
            "{}: runs {rendered:?} binding {misplaced:?}, not sqlc's {:?}",
            query.name, query.text
        ));
    }
}

/// Checks that what a `:one` or `:many` returns follows sqlc's columns: a
/// record's fields one by one (an embed's model is held to its table by
/// [`models_follow_catalog`]), or a bare column. A `:one` reads its row with
/// `.one()` or `.scalar_one()` on SQLAlchemy, or matches the first two of
/// `.fetchmany(2)` on a driver, so no row and a second row both raise, as Go's
/// `ErrNoRows` does, and `None` can only be a NULL.
fn result_follows_sqlc(
    request: &plugin::GenerateRequest,
    query: &plugin::Query,
    function: &StmtFunctionDef,
    body: &[Stmt],
    models: &[Stmt],
    backend: Backend,
    problems: &mut Vec<String>,
) {
    let Some(returns) = function.returns.as_deref() else {
        return problems.push(format!("{}: no return annotation", query.name));
    };
    let item = match (query.cmd.as_str(), returns) {
        (":one", _) => {
            let reads: &[&str] = match backend {
                Backend::SqlAlchemy => &["one", "scalar_one"],
                _ => &["fetchmany"],
            };
            if !calls_method(&function.body, reads) {
                problems.push(format!(
                    "{}: :one reads its row with none of {reads:?}",
                    query.name
                ));
            }
            returns
        }
        (":many", Expr::Subscript(list)) if is_name(&list.value, "list") => &*list.slice,
        (":many", _) => {
            return problems.push(format!(
                "{}: {} returns `{}`",
                query.name,
                query.cmd,
                unparse(returns)
            ));
        }
        _ => return,
    };
    let disagrees = |what: &str| {
        format!(
            "{}: {what} disagrees with sqlc on NULL or array depth",
            query.name
        )
    };
    match record(item, body, models) {
        Some((class, fields)) => {
            let model = item.is_attribute_expr();
            if model != reuses_model(request, &query.columns) {
                problems.push(format!(
                    "{}: returns `{}`, but a model is returned exactly when the result is its table's columns",
                    query.name,
                    unparse(item)
                ));
            }
            if !model {
                let widths: Vec<Option<usize>> = (query.columns.iter())
                    .map(|column| {
                        let embed = column.embed_table.as_ref().filter(|_| is_embed(column))?;
                        Some(table(request, embed).map_or(0, |t| t.columns.len()))
                    })
                    .collect();
                reads_in_order(class, &widths, problems);
            }
            if fields.len() != query.columns.len() {
                return problems.push(format!(
                    "{}: {} fields for {} columns",
                    query.name,
                    fields.len(),
                    query.columns.len()
                ));
            }
            if !model {
                // An embed is spelt as its model's class.
                let spellings: Vec<&str> = (fields.iter().zip(&query.columns))
                    .map(|((_, annotation), column)| match annotation {
                        Expr::Attribute(model) if is_embed(column) => model.attr.as_str(),
                        _ => column.name.as_str(),
                    })
                    .collect();
                let what = format!("{}: class `{}`", query.name, class.name);
                named_after(&what, &fields, &spellings, Repeats::Numbered, problems);
                let comments = query.columns.iter().map(|c| c.comment.as_str());
                documented(&what, class, "", comments, problems);
            }
            for ((name, annotation), column) in fields.into_iter().zip(&query.columns) {
                if !is_embed(column) && Shape::of(annotation) != Shape::from(column) {
                    problems.push(disagrees(&format!("field `{name}`")));
                }
            }
        }
        None => match &query.columns[..] {
            [column] if Shape::of(item) == Shape::from(column) => {}
            _ => problems.push(disagrees("the bare result")),
        },
    }
}

/// Whether `body` calls a method named one of `names`, as `x.one()`.
fn calls_method(body: &[Stmt], names: &[&str]) -> bool {
    struct Calls<'n>(&'n [&'n str], bool);
    impl<'a> Visitor<'a> for Calls<'_> {
        fn visit_expr(&mut self, expr: &'a Expr) {
            if let Expr::Call(call) = expr
                && let Expr::Attribute(method) = &*call.func
            {
                self.1 |= self.0.contains(&method.attr.as_str());
            }
            walk_expr(self, expr);
        }
    }
    let mut calls = Calls(names, false);
    calls.visit_body(body);
    calls.1
}

/// The record `annotation` names, and its fields, if it names one: a class
/// of the query module, or `models.X`.
fn record<'a>(
    annotation: &Expr,
    body: &'a [Stmt],
    models: &'a [Stmt],
) -> Option<(&'a StmtClassDef, Vec<(&'a str, &'a Expr)>)> {
    let (name, scope) = match annotation {
        Expr::Name(name) => (name.id.as_str(), body),
        Expr::Attribute(attribute) if is_name(&attribute.value, "models") => {
            (attribute.attr.as_str(), models)
        }
        _ => return None,
    };
    let class = (scope.iter().filter_map(Stmt::as_class_def_stmt))
        .find(|class| class.name.as_str() == name && is_dataclass(class))?;
    Some((class, fields(class)))
}

/// A dataclass's fields, in order.
fn fields(class: &StmtClassDef) -> Vec<(&str, &Expr)> {
    (class.body.iter().filter_map(Stmt::as_ann_assign_stmt))
        .filter_map(|field| Some((field.target.as_name_expr()?.id.as_str(), &*field.annotation)))
        .collect()
}

fn is_dataclass(class: &StmtClassDef) -> bool {
    (class.decorator_list.iter()).any(|decorator| {
        let callee = match &decorator.expression {
            Expr::Call(call) => &call.func,
            other => other,
        };
        callee.as_attribute_expr().map(|a| a.attr.as_str()) == Some("dataclass")
    })
}

fn is_name(expr: &Expr, id: &str) -> bool {
    expr.as_name_expr()
        .is_some_and(|name| name.id.as_str() == id)
}

/// The SQL of a statement constant: on SQLAlchemy, the string it wraps in
/// `sqlalchemy.text(...)`; on a driver, the string itself.
fn statement_sql(expr: &Expr, backend: Backend) -> Option<String> {
    match backend {
        Backend::SqlAlchemy => text_of(expr),
        _ => Some(expr.as_string_literal_expr()?.value.to_str().into()),
    }
}

/// The string a statement constant wraps in `sqlalchemy.text(...)`, under any
/// methods called on it.
fn text_of(expr: &Expr) -> Option<String> {
    let call = expr.as_call_expr()?;
    match &*call.func {
        Expr::Attribute(method) if method.attr.as_str() == "text" => {
            let string = call.arguments.args.first()?.as_string_literal_expr()?;
            Some(string.value.to_str().into())
        }
        Expr::Attribute(method) => text_of(&method.value),
        _ => None,
    }
}

/// The names a function binds its query's parameters under, and their
/// annotations: its keyword-only parameters, or the fields of the `Params`
/// class its `rows` are made of.
fn parameters<'a>(body: &'a [Stmt], function: &'a StmtFunctionDef) -> Vec<(&'a str, &'a Expr)> {
    if function
        .parameters
        .args
        .iter()
        .any(|p| p.name().as_str() == "rows")
    {
        return params_class(body, function).map(fields).unwrap_or_default();
    }
    (function.parameters.kwonlyargs.iter())
        .filter_map(|p| Some((p.name().as_str(), p.annotation()?)))
        .collect()
}

/// The `Params` class of the module whose `rows` a `:copyfrom` function takes.
fn params_class<'a>(body: &'a [Stmt], function: &StmtFunctionDef) -> Option<&'a StmtClassDef> {
    let rows = (function.parameters.args.iter()).find(|p| p.name().as_str() == "rows")?;
    let iterable = rows.parameter.annotation.as_deref()?.as_subscript_expr()?;
    let class = iterable.slice.as_name_expr()?;
    (body.iter().filter_map(Stmt::as_class_def_stmt)).find(|c| c.name.as_str() == class.id.as_str())
}

/// Checks that `fields`, which `what` describes, are named as the generator
/// names `spellings` (`docs/python-output.md` §3), in order. Two of one type
/// swapped consistently, in the SQL, the signature and `from_row` at once,
/// would pass every other check, and run.
fn named_after(
    what: &str,
    fields: &[(&str, &Expr)],
    spellings: &[&str],
    repeats: Repeats,
    problems: &mut Vec<String>,
) {
    let found: Vec<&str> = fields.iter().map(|(name, _)| *name).collect();
    let expected = models::names(spellings, repeats).unwrap_or_default();
    if found != expected {
        problems.push(format!(
            "{what}: {found:?} are not named after sqlc's {spellings:?}"
        ));
    }
}

/// Checks that a record's docstrings are exactly its comments (§4, rule 5):
/// the class's `comment`, and each field's its own. Only the words are
/// compared; how they are laid out is the golden tests' to pin.
fn documented<'a>(
    what: &str,
    class: &StmtClassDef,
    comment: &'a str,
    fields: impl Iterator<Item = &'a str>,
    problems: &mut Vec<String>,
) {
    let expected: Vec<Vec<&str>> = std::iter::once(comment).chain(fields).map(words).collect();
    let body = &class.body;
    let attributes = (body.iter().enumerate())
        .filter(|(_, stmt)| stmt.is_ann_assign_stmt())
        .map(|(i, _)| docstring(body.get(i + 1)));
    let found: Vec<String> = std::iter::once(docstring(body.first()))
        .chain(attributes)
        .collect();
    if found.iter().map(|d| words(d)).collect::<Vec<_>>() != expected {
        problems.push(format!("{what}: docstrings {found:?} are not its comments"));
    }
}

/// The string `stmt` is, if it is a string statement: a docstring.
fn docstring(stmt: Option<&Stmt>) -> String {
    (stmt.and_then(Stmt::as_expr_stmt))
        .and_then(|stmt| stmt.value.as_string_literal_expr())
        .map_or(String::new(), |string| string.value.to_str().into())
}

fn words(text: &str) -> Vec<&str> {
    text.split_whitespace().collect()
}

/// Whether a result column is `sqlc.embed(t)`.
fn is_embed(column: &plugin::Column) -> bool {
    (column.embed_table.as_ref()).is_some_and(|table| !table.name.is_empty())
}

/// Checks that the models module has one class per catalog enum and table.
///
/// An enum's members are its labels, in order, each named as the generator
/// names it. A table's dataclass has its columns' names, in order, each
/// annotated optional exactly when sqlc says the column admits NULL, and as
/// a list exactly as deep as its array: no type checker or runtime probe can
/// see a non-null column annotated `X | None`, because every value of `X`
/// fits; only sqlc's request can say. Each class and field is documented by
/// its own comment.
fn models_follow_catalog(
    request: &plugin::GenerateRequest,
    models: &[Stmt],
    problems: &mut Vec<String>,
) {
    let schemas = || {
        request
            .catalog
            .iter()
            .flat_map(|catalog| catalog.user_schemas())
    };
    let mut enums: Vec<&plugin::Enum> = schemas().flat_map(|schema| &schema.enums).collect();
    // Each table, and its columns' names and shapes.
    let mut tables: Vec<(&plugin::Table, Vec<(String, Shape)>)> = (schemas())
        .flat_map(|schema| &schema.tables)
        .map(|table| {
            let spellings: Vec<&str> = table.columns.iter().map(|c| c.name.as_str()).collect();
            let names = models::names(&spellings, Repeats::Forbidden).unwrap_or_default();
            (
                table,
                names
                    .into_iter()
                    .zip(table.columns.iter().map(Shape::from))
                    .collect(),
            )
        })
        .collect();

    for class in models.iter().filter_map(Stmt::as_class_def_stmt) {
        let what = format!("models.py: class `{}`", class.name);
        if is_enum(class) {
            let (names, values): (Vec<&str>, Vec<String>) = members(class).into_iter().unzip();
            let Some(sql) = enums.iter().position(|e| e.vals == values) else {
                problems.push(format!("{what} has no SQL enum of its values {values:?}"));
                continue;
            };
            let sql = enums.swap_remove(sql);
            let expected = models::members(&sql.vals).unwrap_or_default();
            if !names.iter().eq(expected.iter().map(|member| &member.name)) {
                problems.push(format!(
                    "{what}: members {names:?} are not named after their labels {values:?}"
                ));
            }
            documented(&what, class, &sql.comment, std::iter::empty(), problems);
            continue;
        }
        let fields = fields(class);
        reads_in_order(class, &vec![None; fields.len()], problems);
        let found: Vec<(String, Shape)> = (fields.iter())
            .map(|(name, annotation)| (name.to_string(), Shape::of(annotation)))
            .collect();
        match tables.iter().position(|(_, columns)| *columns == found) {
            Some(table) => {
                let (table, _) = tables.swap_remove(table);
                let comments = table.columns.iter().map(|c| c.comment.as_str());
                documented(&what, class, &table.comment, comments, problems);
            }
            None => problems.push(format!(
                "{what} has no table with its fields' names, nullability and array depths"
            )),
        }
    }
    for (table, _) in tables {
        let name = table.rel.as_ref().map_or("", |rel| rel.name.as_str());
        problems.push(format!(
            "models.py: table `{name}` has no class with its columns' names, nullability and array depths"
        ));
    }
    for sql in enums {
        problems.push(format!("models.py: enum `{}` has no class", sql.name));
    }
}

fn is_enum(class: &StmtClassDef) -> bool {
    class.bases().iter().map(unparse).eq(["str", "enum.Enum"])
}

/// An enum class's members, as their names and values.
fn members(class: &StmtClassDef) -> Vec<(&str, String)> {
    (class.body.iter().filter_map(Stmt::as_assign_stmt))
        .filter_map(|assign| {
            let [target] = &assign.targets[..] else {
                return None;
            };
            let value = assign.value.as_string_literal_expr()?.value.to_str().into();
            Some((target.as_name_expr()?.id.as_str(), value))
        })
        .collect()
}

/// What a column's annotation says about NULL and arrays.
#[derive(Debug, PartialEq, Eq)]
struct Shape {
    optional: bool,
    depth: usize,
}

impl From<&plugin::Column> for Shape {
    /// What sqlc says of a column.
    fn from(column: &plugin::Column) -> Self {
        Self {
            optional: !column.not_null,
            depth: column.array_dims as usize,
        }
    }
}

impl Shape {
    /// Reads `list[list[T]] | None` as optional, two deep.
    fn of(annotation: &Expr) -> Self {
        fn union(expr: &Expr) -> Vec<&Expr> {
            match expr {
                Expr::BinOp(op) if op.op == Operator::BitOr => {
                    [union(&op.left), union(&op.right)].concat()
                }
                other => vec![other],
            }
        }
        let members = union(annotation);
        let (none, rest): (Vec<_>, Vec<_>) = members
            .into_iter()
            .partition(|member| member.is_none_literal_expr());
        let depth = match rest[..] {
            [Expr::Subscript(list)] if is_name(&list.value, "list") => {
                1 + Self::of(&list.slice).depth
            }
            _ => 0,
        };
        Self {
            optional: !none.is_empty(),
            depth,
        }
    }
}
