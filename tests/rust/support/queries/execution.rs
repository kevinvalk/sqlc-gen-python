//! The invariants of how a query function executes: its own statement, once,
//! binding each parameter as itself, and that statement exactly the SQL sqlc
//! wrote.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use fancy_regex::Regex;

use ruff_python_ast::visitor::{Visitor, walk_expr};
use ruff_python_ast::{Expr, ExprCall, ExprDict, StmtFunctionDef};
use ruff_text_size::{Ranged, TextRange};
use sqlc_gen_python::backend::Backend;
use sqlc_gen_python::engine::{Bind, Engine, Piece, Sql};
use sqlc_gen_python::plugin;

use super::{Takes, uses};
use crate::support::python::{is_name, unparse};

/// Checks that one query's function executes its own statement, exactly once
/// and awaited exactly when async, binding each parameter as itself: under its
/// own name (`{"p": p}`) on SQLAlchemy, or in its own place in a tuple
/// (`(p, q)`), wrapped at most in a JSON type, on a driver; for each row of a
/// `:copyfrom` or batch, as `r.p`, and of the one record, as `params.p`. A
/// function run on its neighbour's statement, or two values of one type
/// swapped, would type-check and run. And that this is all it does with the
/// connection: the caller owns the transaction, which a `conn.commit()` would
/// end.
pub(super) fn runs_its_statement(
    query: &plugin::Query,
    function: &StmtFunctionDef,
    constant: &str,
    parameters: &[(&str, &Expr)],
    engine: Engine,
    (asynchronous, takes): (bool, Takes),
    problems: &mut Vec<String>,
) {
    #[derive(Default)]
    struct Executes<'a> {
        calls: Vec<&'a ExprCall>,
        conns: usize,
        awaited: Vec<TextRange>,
        writes: Vec<TextRange>,
    }
    impl<'a> Visitor<'a> for Executes<'a> {
        fn visit_expr(&mut self, expr: &'a Expr) {
            match expr {
                Expr::Call(call)
                    if call.func.as_attribute_expr().is_some_and(|method| {
                        ["execute", "executemany", "copy"].contains(&method.attr.as_str())
                    }) =>
                {
                    self.calls.push(call)
                }
                // psycopg counts in a pipeline of its own.
                Expr::Attribute(_) if unparse(expr) == "conn.pipeline" => return,
                Expr::Name(name) if name.id.as_str() == "conn" => self.conns += 1,
                Expr::Call(call) if unparse(&call.func) == "copy.write_row" => {
                    self.writes.push(call.range());
                }
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
    // A `COPY` is entered (`async with`), and its rows written awaited.
    let awaited = match unparse(&call.func).ends_with(".copy") {
        false => executes.awaited.contains(&call.range()),
        true => (executes.writes.iter()).all(|write| executes.awaited.contains(write)),
    };
    if awaited != asynchronous {
        problems.push(format!(
            "{}: awaits its statement exactly when async",
            query.name
        ));
    }
    let args = &call.arguments.args;
    // A statement with a `sqlc.slice` is the constant, its placeholders made
    // one per item: `_X.replace(...)`.
    let mut statement = args.first();
    while let Some(Expr::Call(replace)) = statement
        && let Expr::Attribute(method) = &*replace.func
        && method.attr.as_str() == "replace"
    {
        statement = Some(&*method.value);
    }
    // A batch on psycopg asks for each row's result: `returning=True`.
    let keywords: Vec<String> = call
        .arguments
        .keywords
        .iter()
        .map(|k| unparse(&k.value))
        .collect();
    let returning = (call.arguments.keywords.iter()).all(|k| {
        k.arg
            .as_ref()
            .is_some_and(|arg| arg.as_str() == "returning")
    }) && keywords.iter().all(|value| value == "True");
    if !statement.is_some_and(|first| is_name(first, constant)) || !returning {
        let args: Vec<String> = args.iter().map(unparse).collect();
        return problems.push(format!(
            "{}: executes ({}), not its own statement `{constant}`",
            query.name,
            args.join(", ")
        ));
    }

    let rows = takes == Takes::Rows;
    let value = |name: &str| takes.value(name);
    // Each parameter as it is bound: by name, or by position, a slice's
    // items each in a place of its own. psycopg binds `$N` by `N`; PyMySQL's
    // and `sqlite3`'s own placeholders bind in the text's order.
    let names: Vec<&str> = parameters.iter().map(|(name, _)| *name).collect();
    let Some(pieces) = cut(query, engine, &names) else {
        return problems.push(format!("{}: sqlc's text does not cut", query.name));
    };
    let mut placed: Vec<(i32, String)> = Vec::new();
    for (at, piece) in pieces.0.iter().enumerate() {
        let (number, value) = match piece {
            Piece::Text(_) => continue,
            Piece::Param { number, name, .. } => (number, value(name)),
            Piece::Slice { number, name, .. } => (number, format!("*{}", value(name))),
        };
        let number = match engine {
            Engine::PostgreSql => *number,
            _ => at as i32,
        };
        if !placed.iter().any(|(n, _)| *n == number) {
            placed.push((number, value));
        }
    }
    placed.sort_by_key(|(number, _)| *number);
    let mut expected: Vec<(String, String)> = (placed.into_iter().enumerate())
        .map(|(i, (_, value))| (format!("${}", i + 1), value))
        .collect();
    let bound = match (&args[1..], rows) {
        ([], false) => Some(Vec::new()),
        ([Expr::Dict(dict)], _) => Some(pairs(dict)),
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
        // psycopg's `COPY`: each row written as a tuple.
        ([], true) => {
            #[derive(Default)]
            struct Rows<'a>(Vec<&'a [Expr]>);
            impl<'a> Visitor<'a> for Rows<'a> {
                fn visit_expr(&mut self, expr: &'a Expr) {
                    if let Expr::Call(call) = expr
                        && unparse(&call.func) == "copy.write_row"
                        && let [Expr::Tuple(tuple)] = &call.arguments.args[..]
                    {
                        self.0.push(&tuple.elts);
                    }
                    walk_expr(self, expr);
                }
            }
            let mut rows = Rows::default();
            rows.visit_body(&function.body);
            match rows.0[..] {
                [row] => Some(positions(row)),
                _ => None,
            }
        }
        _ => None,
    };
    if bound
        .as_ref()
        .is_some_and(|bound| bound.iter().all(|(key, _)| !key.starts_with('$')))
    {
        expected = (parameters.iter())
            .map(|(name, _)| (name.to_string(), value(name)))
            .collect();
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

/// Checks that one query's statement runs exactly the SQL sqlc wrote, given
/// the names it binds its parameters under, in sqlc's order. A driver is
/// given sqlc's text itself; SQLAlchemy's goes through what SQLAlchemy does
/// with it, with its own expressions copied verbatim: find the binds, which
/// must be exactly the function's parameters; render each as its `$N`; and
/// unescape `\:`. The result must be sqlc's `query.text`, byte for byte.
pub(super) fn sql_round_trips(
    query: &plugin::Query,
    sql: &str,
    parameters: &[(&str, &Expr)],
    backend: Backend,
    engine: Engine,
    problems: &mut Vec<String>,
) {
    if backend == Backend::Psycopg && query.cmd == ":copyfrom" {
        // psycopg copies the rows into sqlc's table and columns.
        let quote = |name: &str| format!("\"{}\"", name.replace('"', "\"\""));
        let table = query.insert_into_table.clone().unwrap_or_default();
        let table = match table.schema.as_str() {
            "" => quote(&table.name),
            schema => format!("{}.{}", quote(schema), quote(&table.name)),
        };
        let columns: Vec<String> = (query.params.iter())
            .map(|p| {
                let column = p.column.clone().unwrap_or_default();
                match column.original_name.as_str() {
                    "" => quote(&column.name),
                    original => quote(original),
                }
            })
            .collect();
        let copy = format!("COPY {table} ({}) FROM STDIN", columns.join(", "));
        if sql != copy {
            problems.push(format!("{}: copies with {sql:?}, not {copy:?}", query.name));
        }
        return;
    }
    if backend != Backend::SqlAlchemy {
        // PyMySQL formats the statement with `%`, as `sql % args`: each `%s`
        // is a placeholder, `?` to MySQL, and `%%` a `%`.
        let sql = match (backend, query.params.is_empty()) {
            (Backend::Pymysql, false) => {
                let mut formatted = String::new();
                let mut chars = sql.chars();
                while let Some(c) = chars.next() {
                    match (c, (c == '%').then(|| chars.next()).flatten()) {
                        ('%', Some('%')) => formatted.push('%'),
                        ('%', Some('s')) => formatted.push('?'),
                        ('%', other) => {
                            let other = other.map(String::from).unwrap_or_default();
                            return problems.push(format!(
                                "{}: `%{other}` is no format PyMySQL reads",
                                query.name
                            ));
                        }
                        (c, _) => formatted.push(c),
                    }
                }
                formatted
            }
            _ => sql.to_string(),
        };
        // `sqlite3` binds a bare `?` per value, in the text's order.
        let text = match (backend, cut(query, engine, &[])) {
            (Backend::Sqlite3, Some(pieces)) => (pieces.0.iter())
                .map(|piece| match piece {
                    Piece::Text(text) => text.clone(),
                    Piece::Param { .. } => "?".into(),
                    Piece::Slice { marker, .. } => format!("{marker}?"),
                })
                .collect(),
            _ => query.text.clone(),
        };
        if sql != text {
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
        (include_str!("../sqlalchemy.re").lines())
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
    let Some(pieces) = cut(query, engine, &names) else {
        return problems.push(format!("{}: sqlc's text does not cut", query.name));
    };
    let mut placeholders = (pieces.0.into_iter()).filter_map(|piece| match piece {
        Piece::Param {
            placeholder, name, ..
        } => Some((placeholder, name)),
        // An expanding bind is its parentheses too.
        Piece::Slice {
            marker,
            placeholder,
            name,
            ..
        } => Some((format!("({marker}{placeholder})"), name)),
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

/// sqlc's text of `query`, cut at its placeholders as `engine` reads them,
/// each named as `names`, the function's parameters, name it (or `?`).
fn cut(query: &plugin::Query, engine: Engine, names: &[&str]) -> Option<Sql> {
    let uses = uses(query);
    let firsts: Vec<usize> = (0..uses.len()).filter(|&i| uses[i] == i).collect();
    let binds: Vec<Bind> = (query.params.iter().zip(&uses).enumerate())
        .map(|(i, (param, first))| {
            let at = firsts.iter().position(|f| f == first);
            let column = param.column.as_ref();
            Bind {
                number: match engine {
                    Engine::PostgreSql => param.number,
                    _ => i as i32 + 1,
                },
                name: at.and_then(|at| names.get(at)).copied().unwrap_or("?"),
                slice: (column.filter(|c| c.is_sqlc_slice)).map(|c| c.name.as_str()),
            }
        })
        .collect();
    engine.sql(&query.text, &binds).ok()
}
