use std::collections::BTreeSet;
use std::sync::LazyLock;

use fancy_regex::Regex;

use ruff_python_ast::visitor::{Visitor, walk_expr};
use ruff_python_ast::{Expr, ExprCall, ExprDict, StmtFunctionDef};
use sqlc_gen_python::backend::Backend;
use sqlc_gen_python::config::Config;
use sqlc_gen_python::engine::{Bind, Engine, Piece, Sql};
use sqlc_gen_python::plugin;

use super::{Takes, uses};
use crate::support::python::{is_name, unparse};

/// A query's function executes its own statement exactly once and binds each parameter as itself: by its own name
/// (`{"p": p}`) on SQLAlchemy, or in its own place in a tuple (`(p, q)`) on a driver, through any JSON wrapper or encoder;
/// as `r.p` for each row of a `:copyfrom` or batch, and as `params.p` for the one record. A function run on its
/// neighbour's statement, or with two values of one type swapped, would type-check and run. It does nothing else with
/// the connection either, as the caller owns the transaction a `connection.commit()` would end.
pub(super) fn runs_its_statement(
    query: &plugin::Query,
    function: &StmtFunctionDef,
    constant: &str,
    parameters: &[(&str, &Expr)],
    (config, engine): (&Config, Engine),
    takes: Takes,
    problems: &mut Vec<String>,
) {
    #[derive(Default)]
    struct Executes<'a> {
        calls: Vec<&'a ExprCall>,
        connections: usize,
    }
    impl<'a> Visitor<'a> for Executes<'a> {
        fn visit_expr(&mut self, expr: &'a Expr) {
            match expr {
                Expr::Call(call)
                    if call.func.as_attribute_expr().is_some_and(|method| {
                        ["execute", "executemany", "copy", "stream"].contains(&method.attr.as_str())
                    }) =>
                {
                    self.calls.push(call)
                }
                // psycopg runs a batch in a pipeline of its own, which is no second use of the connection.
                Expr::Attribute(_) if unparse(expr) == "connection.pipeline" => return,
                Expr::Name(name) if name.id.as_str() == "connection" => self.connections += 1,
                _ => {}
            }
            walk_expr(self, expr);
        }
    }

    let mut executes = Executes::default();
    executes.visit_body(&function.body);
    let call = match (&executes.calls[..], executes.connections) {
        ([call], 1) => call,
        // Both arms must execute our statement. One takes all the bindings and returns before the fallback
        // that sums a count for each of those same rows.
        ([batch, one], 3) if config.backend == Backend::SqlAlchemy && query.cmd == ":copyfrom" => {
            let correct = unparse(&one.func) == "connection.execute"
                && matches!(&one.arguments.args[..], [sql, row] if is_name(sql, constant) && is_name(row, "row"))
                && one.arguments.keywords.is_empty()
                && function.body.iter().any(|stmt| {
                    stmt.as_if_stmt().is_some_and(|guard| {
                        unparse(&guard.test) == "connection.dialect.supports_sane_multi_rowcount"
                            && matches!(guard.body.as_slice(), [stmt] if stmt.is_return_stmt())
                    })
                })
                && function.body.iter().any(|stmt| {
                    stmt.as_for_stmt().is_some_and(|loop_| {
                        is_name(&loop_.target, "row")
                            && is_name(&loop_.iter, "params")
                            && matches!(loop_.body.as_slice(), [stmt] if stmt.is_aug_assign_stmt())
                    })
                });
            if !correct {
                return problems.push(format!(
                    "{}: does not count each execution on a driver without a bulk count",
                    query.name
                ));
            }
            batch
        }
        _ => {
            return problems.push(format!(
                "{}: executes {} statements, and reads `connection` {} times, not only to execute its SQL",
                query.name,
                executes.calls.len(),
                executes.connections
            ));
        }
    };

    // With a `sqlc.slice` the statement is the constant with its placeholders made one per item: `_X.replace(...)`.
    let arguments = &call.arguments.args;
    let mut statement = arguments.first();
    while let Some(Expr::Call(replace)) = statement
        && let Expr::Attribute(method) = &*replace.func
        && method.attr.as_str() == "replace"
    {
        statement = Some(&*method.value);
    }

    // A batch on psycopg asks for each row's result, `returning=True`, and a stream on SQLAlchemy for a server-side
    // cursor.
    let keywords: Vec<String> = (call.arguments.keywords.iter())
        .map(|k| {
            format!(
                "{}={}",
                k.arg.as_ref().map_or("", |arg| arg.as_str()),
                unparse(&k.value)
            )
        })
        .collect();
    let options = keywords
        .iter()
        .all(|keyword| ["returning=True", r#"execution_options={"stream_results": True}"#].contains(&keyword.as_str()));
    if !statement.is_some_and(|first| is_name(first, constant)) || !options {
        let arguments: Vec<String> = arguments.iter().map(unparse).collect();
        return problems.push(format!(
            "{}: executes ({}), not its own statement `{constant}`",
            query.name,
            arguments.join(", ")
        ));
    }

    let takes_rows = takes == Takes::Rows;
    let value = |name: &str| takes.value(name);

    // We expect each parameter by name, or by position with a slice's items each in a place of its own. psycopg binds
    // `$N` by `N`, while PyMySQL's and `sqlite3`'s own placeholders bind in the order of the text.
    let names: Vec<&str> = parameters.iter().map(|(name, _)| *name).collect();
    let Some(pieces) = cut(query, engine, &names) else {
        return problems.push(format!("{}: sqlc's text does not cut", query.name));
    };
    let mut placed: Vec<(i32, String)> = Vec::new();
    for (position, piece) in pieces.0.iter().enumerate() {
        let (number, value) = match piece {
            Piece::Text(_) => continue,
            Piece::Param { number, name, .. } => (number, value(name)),
            Piece::Slice { number, name, .. } => (number, format!("*{}", value(name))),
        };
        let number = match engine {
            Engine::PostgreSql => *number,
            _ => position as i32,
        };
        if !placed.iter().any(|(n, _)| *n == number) {
            placed.push((number, value));
        }
    }
    placed.sort_by_key(|(number, _)| *number);
    let mut expected: Vec<(String, String)> = (placed.into_iter().enumerate())
        .map(|(i, (_, value))| (format!("${}", i + 1), value))
        .collect();

    let encoders = config
        .overrides
        .iter()
        .filter_map(|rule| rule.to_db.as_ref().map(|call| unparse(&call.0.expr)))
        .collect::<BTreeSet<_>>();
    let positions = |items: &[Expr]| {
        items
            .iter()
            .enumerate()
            .map(|(i, item)| (format!("${}", i + 1), value_bound(item, &encoders)))
            .collect::<Vec<_>>()
    };
    let pairs = |dict: &ExprDict| {
        dict.items
            .iter()
            .map(|item| {
                let key = item.key.as_ref().and_then(Expr::as_string_literal_expr);
                (
                    key.map_or("?".into(), |key| key.value.to_str().into()),
                    value_bound(&item.value, &encoders),
                )
            })
            .collect::<Vec<_>>()
    };
    let bound = match (&arguments[1..], takes_rows) {
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

    // A dict binds by name, so then we expect each parameter under its own name.
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

/// The value a bind reads through its JSON wrapper, configured encoder, NULL guard and array comprehensions.
fn value_bound(item: &Expr, encoders: &BTreeSet<String>) -> String {
    if let Expr::Starred(starred) = item {
        return format!("*{}", value_bound(&starred.value, encoders));
    }
    converted(item, encoders).unwrap_or_else(|| unparse(item))
}

fn converted(item: &Expr, encoders: &BTreeSet<String>) -> Option<String> {
    match item {
        Expr::Call(call)
            if call.arguments.keywords.is_empty()
                && (["Json", "Jsonb"].iter().any(|name| is_name(&call.func, name))
                    || encoders.contains(&unparse(&call.func)))
                && let [value] = call.arguments.args.as_ref() =>
        {
            Some(value_bound(value, encoders))
        }
        Expr::If(choice) if choice.body.is_none_literal_expr() => {
            let value = converted(&choice.orelse, encoders)?;
            (unparse(&choice.test) == format!("{value} is None")).then_some(value)
        }
        Expr::ListComp(list)
            if let [each] = list.generators.as_slice()
                && each.ifs.is_empty()
                && !each.is_async
                && converted(&list.elt, encoders).is_some_and(|value| value == unparse(&each.target)) =>
        {
            Some(unparse(&each.iter))
        }
        _ => None,
    }
}

/// A query's statement runs exactly the SQL sqlc wrote, given the names it binds its parameters under in sqlc's order.
/// A driver gets sqlc's text itself. SQLAlchemy's text goes through what SQLAlchemy does with it, with its own
/// expressions copied verbatim: we find the binds, which must be exactly the function's parameters, render each as its
/// `$N` and unescape `\:`, and the result must be sqlc's `query.text` byte for byte.
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
            .map(|parameter| {
                let column = parameter.column.clone().unwrap_or_default();
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
        // PyMySQL formats the statement with `%` as `sql % args`, so each `%s` is a placeholder (`?` to MySQL) and `%%`
        // a `%`.
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
                            return problems.push(format!("{}: `%{other}` is no format PyMySQL reads", query.name));
                        }
                        (c, _) => formatted.push(c),
                    }
                }
                formatted
            }
            _ => sql.to_string(),
        };

        // `sqlite3` binds a bare `?` per value, in the order of the text.
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
            problems.push(format!("{}: runs {sql:?}, not sqlc's {:?}", query.name, query.text));
        }
        return;
    }
    // These are SQLAlchemy's `TextClause._bind_params_regex` and `compiler.BIND_PARAMS_ESC`, verbatim, and the runtime
    // suite checks our copy against the SQLAlchemy it runs (`test_contract.py`). NOTE: We spell `\w` out as Python's,
    // a letter, a number or `_` (which `test_contract.py` checks over every code point), as this engine's also takes
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
        Piece::Param { placeholder, name, .. } => Some((placeholder, name)),
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

/// sqlc's text of `query` cut at its placeholders as `engine` reads them, each named after the function parameter in
/// `names` it binds (or `?`).
fn cut(query: &plugin::Query, engine: Engine, names: &[&str]) -> Option<Sql> {
    let uses = uses(query);
    let firsts: Vec<usize> = (0..uses.len()).filter(|&i| uses[i] == i).collect();
    let binds: Vec<Bind> = (query.params.iter().zip(&uses).enumerate())
        .map(|(i, (parameter, first))| {
            let at = firsts.iter().position(|f| f == first);
            let column = parameter.column.as_ref();
            Bind {
                argument: at.unwrap_or(i),
                number: match engine {
                    Engine::PostgreSql => parameter.number,
                    _ => i as i32 + 1,
                },
                name: at.and_then(|at| names.get(at)).copied().unwrap_or("?"),
                slice: (column.filter(|c| c.is_sqlc_slice)).map(|c| c.name.as_str()),
            }
        })
        .collect();
    engine.sql(&query.text, &binds).ok()
}
