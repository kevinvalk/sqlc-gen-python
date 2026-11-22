use std::collections::BTreeSet;

use ruff_python_ast::token::TokenKind;
use ruff_python_ast::visitor::{Visitor, walk_expr, walk_stmt};
use ruff_python_ast::{Expr, ModModule, Stmt};
use ruff_python_parser::{Parsed, parse_module};
use ruff_text_size::Ranged;

use super::python::{docstring, is_enum, is_error, unparse, unparse_stmt, unwrapped};
use sqlc_gen_python::config::Records;

pub(super) const JSON_DOCUMENT: &str = "Mapping[str, object] | list[Any] | tuple[object, ...] | float";

pub fn check_module(source: &str) -> Vec<String> {
    let parsed = parse_module(source).expect("planted Python");
    module_problems(&parsed, source, false, Records::Dataclass)
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
    ) -> int | None: ...

    async def fetchmany(self, size: int) -> Sequence[Sequence[Any]]: ...

    def __aiter__(self) -> AsyncIterator[Sequence[Any]]: ...",
    "def _typed(cursor: asyncmy.cursors.Cursor) -> _Cursor:
    return cursor",
];

/// The driver returns each PostgreSQL array as one row value. Only its leaves may have an unknown Python type here.
pub(super) const CHECKED_ARRAY: &[&str] = &[r#"def _checked_array(
    value: object, dimensions: int, *, list_leaves: bool = False
) -> list[Any]:
    if isinstance(value, list):
        items = typing.cast("list[object]", value)
        if dimensions != 1:
            for child in items:
                _checked_array(child, dimensions - 1, list_leaves=list_leaves)
            return items
        if (list_leaves or not items) or not isinstance(items[0], list):
            return items
    message = "PostgreSQL returned an array with a different number of dimensions than declared"
    raise TypeError(message)"#];

/// The prelude `body` holds and where it starts. A query module holds one between its imports and its queries when its
/// library's stubs need it, and only these statements, to the letter, may be private, annotate with an `Any` that is not
/// the library's own, or be a class that is no record.
pub(super) fn prelude(body: &[Stmt], source: &str) -> Option<(usize, &'static [&'static str])> {
    [PYMYSQL, ASYNCMY, CHECKED_ARRAY].into_iter().find_map(|prelude| {
        let held = |stmts: &[Stmt]| (stmts.iter().map(|stmt| &source[stmt.range()])).eq(prelude.iter().copied());
        Some((body.windows(prelude.len()).position(held)?, prelude))
    })
}

pub(super) fn module_problems(
    parsed: &Parsed<ModModule>,
    source: &str,
    accepts_any: bool,
    records: Records,
) -> Vec<String> {
    let module = parsed.syntax();
    let mut own = module.body.clone();
    if let Some((start, prelude)) = prelude(&own, source) {
        own.drain(start..start + prelude.len());
    }

    let mut problems = Vec::new();
    only_defines(&own, &mut problems);
    comments_are_the_header(parsed, source, &mut problems);
    scopes("module", &own, &mut problems);
    annotations_are_precise(&own, accepts_any, &mut problems);
    records_are_values(&own, records, &mut problems);
    problems
}

/// Importing a module only defines it: it imports, declares classes and functions, and binds each statement to a
/// constant. Anything else on import, like registering a driver's adapter, would change how the whole application reads
/// its rows. The query checks pin what a statement calls, and [`records_are_values`] a class's decorators and bases.
fn only_defines(body: &[Stmt], problems: &mut Vec<String>) {
    fn is_statement(value: &Expr) -> bool {
        match value {
            Expr::StringLiteral(_) => true,
            Expr::Call(call) => {
                unparse(&call.func) == "sqlalchemy.text"
                    || (call.func.as_attribute_expr()).is_some_and(|method| is_statement(&method.value))
            }
            _ => false,
        }
    }

    for stmt in body {
        let defines = match stmt {
            Stmt::Import(_) | Stmt::ImportFrom(_) | Stmt::ClassDef(_) => true,
            Stmt::If(branch) => {
                unparse(&branch.test) == "typing.TYPE_CHECKING"
                    && branch.elif_else_clauses.is_empty()
                    && branch
                        .body
                        .iter()
                        .all(|stmt| matches!(stmt, Stmt::Import(_) | Stmt::ImportFrom(_)))
            }
            Stmt::FunctionDef(function) => (function.decorator_list.iter())
                .all(|decorator| unparse(&decorator.expression).starts_with("contextlib.")),
            Stmt::AnnAssign(constant) => constant.value.as_deref().is_some_and(is_statement),
            _ => false,
        };
        if !defines {
            let written = unparse_stmt(stmt);
            let line = written.lines().next().unwrap_or_default();
            problems.push(format!("`{line}` does more than define on import"));
        }
    }
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

/// Every class but an enum and an error is a record, of the one kind the options ask for: exactly the one dataclass
/// decorator that makes it frozen, slotted and keyword-only, or a `pydantic.BaseModel` whose configuration, first after
/// its docstring, makes it frozen, forbids a field it does not have, and checks a type pydantic does not know as an
/// instance of its class. A record of results (it has a `from_row`) has no default, so an omitted value is a type error and
/// never a NULL; a record of parameters defaults only a nullable one, to `None`, as a filter or a partial update leaves
/// it out.
fn records_are_values(body: &[Stmt], records: Records, problems: &mut Vec<String>) {
    for class in body.iter().filter_map(Stmt::as_class_def_stmt) {
        if is_enum(class) || is_error(class) {
            continue;
        }

        let decorators: Vec<String> = (class.decorator_list.iter())
            .map(|decorator| unparse(&decorator.expression))
            .collect();
        let bases: Vec<String> = class.bases().iter().map(unparse).collect();
        let first = (class.body.iter())
            .find(|stmt| docstring(Some(stmt)).is_empty())
            .map(unparse_stmt);
        let (is_record, kind) = match records {
            Records::Dataclass => {
                let decorator = "dataclasses.dataclass(frozen=True, slots=True, kw_only=True)";
                (decorators == [decorator] && bases.is_empty(), format!("`@{decorator}`"))
            }
            Records::Pydantic => {
                let config = "model_config = pydantic.ConfigDict(frozen=True, extra=\"forbid\", arbitrary_types_allowed=True, protected_namespaces=())";
                let is_model = decorators.is_empty() && bases == ["pydantic.BaseModel"];
                (
                    is_model && first.as_deref() == Some(config),
                    format!("`pydantic.BaseModel` of `{config}`"),
                )
            }
        };
        if !is_record {
            problems.push(format!("`{}` is not exactly a {kind}", class.name));
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

/// No annotation mentions `Any` but the library's own: the row `from_row` reads (`row: Sequence[Any]`), what
/// `:execresult` returns (`sqlalchemy.CursorResult[Any]`, a psycopg cursor of tuples) and a psycopg connection,
/// whatever rows it makes. `Any` passes every type checker, so it is the one annotation the static gate cannot catch
/// being wrong, and the generator must say what a column or parameter is or fail.
fn annotations_are_precise(body: &[Stmt], accepts_any: bool, problems: &mut Vec<String>) {
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

    // A JSON parameter's `list[Any]` admits a list of anything, as `list` is invariant, and is only ever encoded
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
        if !mentions_any.0 || accepts_any {
            continue;
        }
        let written = unparse(unwrapped(annotation));
        let bare = written.strip_suffix(" | None").unwrap_or(&written);
        let mut present = String::from(JSON_DOCUMENT);
        let mut nullable = format!("{JSON_DOCUMENT} | None");
        let is_document = bare == JSON_DOCUMENT
            || (0..6).any(|_| {
                present = format!("list[{present}]");
                nullable = format!("list[{nullable}]");
                bare == format!("{nullable} | {present}")
            });
        if !is_document && !drivers_own.contains(&written.as_str()) {
            problems.push(format!("{what} is annotated with `Any`"));
        }
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
    default.is_none_literal_expr() && unparse(unwrapped(annotation)).ends_with("| None")
}
