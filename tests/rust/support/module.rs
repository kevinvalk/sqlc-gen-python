use std::collections::BTreeSet;

use ruff_python_ast::token::TokenKind;
use ruff_python_ast::visitor::{Visitor, walk_expr, walk_stmt};
use ruff_python_ast::{Expr, ModModule, Stmt};
use ruff_python_formatter::{PyFormatOptions, format_module_source};
use ruff_python_parser::{Parsed, parse_module};
use ruff_python_stdlib::builtins::is_python_builtin;
use ruff_text_size::Ranged;

use super::python::{is_enum, unparse};

pub fn check_module(source: &str) -> Vec<String> {
    match parse_module(source) {
        Ok(parsed) => module_problems(&parsed, source),
        Err(error) => vec![format!("does not parse: {error}")],
    }
}

/// PyMySQL's connection is generic in its stubs only, so we name it once for a type checker and once for Python.
pub(super) const PYMYSQL: &[&str] = &["if typing.TYPE_CHECKING:
    _Connection = pymysql.Connection[Any]
else:
    _Connection = pymysql.Connection"];

/// asyncmy's stubs leave these methods untyped, so we view a cursor through a protocol of our own.
///
/// NOTE: This is a workaround for asyncmy's stubs, and the "Workarounds" of AGENTS.md say when and how it goes.
pub(super) const ASYNCMY: &[&str] = &[
    "class _Cursor(typing.Protocol):
    async def execute(self, query: str, args: Sequence[Any] | None = None) -> int: ...

    async def executemany(
        self, query: str, args: Sequence[Sequence[Any]]
    ) -> object: ...

    async def fetchmany(self, size: int) -> Sequence[Sequence[Any]]: ...",
    "def _typed(cursor: asyncmy.cursors.Cursor) -> _Cursor:
    return cursor",
];

/// The prelude `body` holds and where it starts. A query module holds one between its imports and its queries when its
/// library's stubs need it, and only these statements, to the
/// letter, may be private, annotate with an `Any` that is not the library's own, or be a class that is no record.
pub(super) fn prelude(body: &[Stmt], source: &str) -> Option<(usize, &'static [&'static str])> {
    [PYMYSQL, ASYNCMY].into_iter().find_map(|prelude| {
        let held = |stmts: &[Stmt]| (stmts.iter().map(|stmt| &source[stmt.range()])).eq(prelude.iter().copied());
        Some((body.windows(prelude.len()).position(held)?, prelude))
    })
}

pub(super) fn module_problems(parsed: &Parsed<ModModule>, source: &str) -> Vec<String> {
    let module = parsed.syntax();
    let mut own = module.body.clone();
    if let Some((start, prelude)) = prelude(&own, source) {
        own.drain(start..start + prelude.len());
    }

    let mut problems = Vec::new();
    match format_module_source(source, PyFormatOptions::default()) {
        Ok(formatted) if formatted.as_code() == source => {}
        Ok(_) => problems.push("is not a fixpoint of ruff's formatter".into()),
        Err(error) => problems.push(format!("does not format: {error}")),
    }
    comments_are_the_header(parsed, source, &mut problems);
    scopes("module", &own, &mut problems);
    reads_resolve(&module.body, &mut problems);
    annotations_are_precise(&own, &mut problems);
    records_are_values(&own, &mut problems);
    problems
}

/// The only comments are the header the module opens with, and its one comment for a tool is
/// `# ruff: noqa: S105`. Any other would silence a tool the static gate obeys: a `# type: ignore` both type checkers on
/// its line, a `# noqa` ruff.
fn comments_are_the_header(parsed: &Parsed<ModModule>, source: &str, problems: &mut Vec<String>) {
    let mut is_after_header = false;
    for token in parsed.tokens().iter() {
        let comment = &source[token.range()];
        match token.kind() {
            TokenKind::Comment if is_after_header => {
                problems.push(format!("`{comment}` is a comment after the header"));
            }
            TokenKind::Comment
                if (comment.contains("noqa") || comment.contains("type:")) && comment != "# ruff: noqa: S105" =>
            {
                problems.push(format!("`{comment}` silences a tool"));
            }
            TokenKind::Comment | TokenKind::NonLogicalNewline | TokenKind::Newline => {}
            _ => is_after_header = true,
        }
    }
}

/// Every class but an enum and an error is a record: exactly the one dataclass decorator that makes it frozen, slotted
/// and keyword-only. A record of results (it has a `from_row`) has no default, so an omitted value is a type error and
/// never a NULL; a record of parameters defaults only a nullable one, to `None`, as a filter or a partial update leaves
/// it out.
fn records_are_values(body: &[Stmt], problems: &mut Vec<String>) {
    for class in body.iter().filter_map(Stmt::as_class_def_stmt) {
        if is_enum(class) || class.bases().iter().map(unparse).eq(["LookupError"]) {
            continue;
        }

        let record_decorator = "dataclasses.dataclass(frozen=True, slots=True, kw_only=True)";
        let decorators: Vec<String> = (class.decorator_list.iter())
            .map(|decorator| unparse(&decorator.expression))
            .collect();
        if decorators != [record_decorator] || !class.bases().is_empty() {
            problems.push(format!("`{}` is not exactly a `@{record_decorator}`", class.name));
        }
        let is_results =
            (class.body.iter().filter_map(Stmt::as_function_def_stmt)).any(|f| f.name.as_str() == "from_row");
        for field in class.body.iter().filter_map(Stmt::as_ann_assign_stmt) {
            let Some(default) = &field.value else {
                continue;
            };
            if is_results || !is_none_for_nullable(&field.annotation, default) {
                problems.push(format!("`{}.{}` has a default", class.name, unparse(&field.target)));
            }
        }
    }
}

/// The parser already proved every name a valid identifier, keywords included, so we check what it accepts and Python
/// then gets wrong: a name bound twice in one scope silently loses the first (a field, an enum member, a class
/// shadowing an import), and a leading underscore makes a name private (a double one is mangled, and `enum` reserves
/// `_sunder_` names).
fn scopes(scope: &str, body: &[Stmt], problems: &mut Vec<String>) {
    bound(scope, body.iter().flat_map(bindings), problems);
    for stmt in body {
        match stmt {
            Stmt::ClassDef(class) => scopes(&format!("class {}", class.name), &class.body, problems),
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
        let is_dunder = name.len() > 4 && name.starts_with("__") && name.ends_with("__");

        // A module's `_UPPER_SNAKE` constant, such as the statement a query function runs, is an implementation detail:
        // the underscore keeps it out of the public interface (`from m import *`, completion) and out of the way of the
        // function named after the same query.
        let is_module_constant = scope == "module"
            && (name.strip_prefix('_')).is_some_and(|rest| {
                rest.starts_with(|c: char| c.is_ascii_uppercase())
                    && rest
                        .chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
            });

        if name.starts_with('_') && !is_dunder && !is_module_constant {
            problems.push(format!("{scope}: `{name}` is private"));
        }
    }
}

/// Every name the module reads is bound by an earlier statement, or is a builtin of Python 3.10, the oldest we support.
/// Class bodies and their annotations run on import, so a name read too early or never bound (a model annotated with
/// a class we did not generate) is a `NameError` the moment anyone imports the module.
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

/// No annotation mentions `Any` but the library's own: the row `from_row` reads (`row: Sequence[Any]`), what
/// `:execresult` returns (`sqlalchemy.CursorResult[Any]`, a psycopg cursor of tuples) and a psycopg connection,
/// whatever rows it makes. `Any` passes every type checker, so it is the one annotation the static gate cannot catch
/// being wrong, and the generator must say what a column or parameter is or fail.
fn annotations_are_precise(body: &[Stmt], problems: &mut Vec<String>) {
    /// Each annotation with a description for the message, and the spellings of it that are the driver's own.
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
                            let drivers_own = match (name.as_str(), parameter.name().as_str()) {
                                ("from_row", "row") => &["Sequence[Any]"][..],
                                (_, "connection") => &["psycopg.Connection[Any]", "psycopg.AsyncConnection[Any]"],
                                _ => &[],
                            };
                            let what = format!("parameter `{}` of `{name}`", parameter.name());
                            self.0.push((what, annotation, drivers_own));
                        }
                    }
                    if let Some(returns) = &function.returns {
                        let what = format!("the result of `{name}`");
                        let drivers_own = &[
                            "sqlalchemy.CursorResult[Any]",
                            "psycopg.RawCursor[tuple[Any, ...]]",
                            "psycopg.AsyncRawCursor[tuple[Any, ...]]",
                        ];
                        self.0.push((what, returns, drivers_own));
                    }
                }
                _ => {}
            }
            walk_stmt(self, stmt);
        }
    }

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
    for (what, annotation, drivers_own) in annotations.0 {
        let mut mentions_any = MentionsAny::default();
        mentions_any.visit_expr(annotation);
        if mentions_any.0 && !drivers_own.contains(&unparse(annotation).as_str()) {
            problems.push(format!("{what} is annotated with `Any`"));
        }
    }
}

/// The names a statement reads on import, in order. A function's signature runs when it is defined but its body only
/// when called, so we skip the body.
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

/// The names a statement binds in the scope it appears in.
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
        Stmt::If(stmt) => (stmt.body.iter())
            .chain(stmt.elif_else_clauses.iter().flat_map(|clause| &clause.body))
            .flat_map(bindings)
            .collect(),
        Stmt::ClassDef(class) => vec![class.name.as_str()],
        Stmt::FunctionDef(function) => vec![function.name.as_str()],
        Stmt::Assign(assign) => assign.targets.iter().filter_map(target).collect(),
        Stmt::AnnAssign(assign) => target(&assign.target).into_iter().collect(),
        _ => Vec::new(),
    }
}

/// Whether `default` is `None`, for an annotation that admits it.
pub(super) fn is_none_for_nullable(annotation: &Expr, default: &Expr) -> bool {
    default.is_none_literal_expr() && unparse(annotation).ends_with("| None")
}
