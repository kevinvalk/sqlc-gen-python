use ruff_python_ast::visitor::{Visitor, walk_expr};
use ruff_python_ast::{Expr, Stmt, StmtFunctionDef};
use sqlc_gen_python::api::names::{Binding, Repeats};
use sqlc_gen_python::backend::Backend;
use sqlc_gen_python::config::Config;
use sqlc_gen_python::engine::Engine;
use sqlc_gen_python::plugin;

use crate::support::python::{Shape, is_name, record, strip_optional, unparse};
use crate::support::records::{documented, named_after, reads_in_order};

/// What a `:one` or `:many` returns follows sqlc's columns: a record's fields one by one (`models_follow_catalog` holds
/// an embed's model to its table), or a bare column. A `:one` reads its row so that a second row raises, and no row is
/// `None` or raises where the query does: `.one_or_none()` or `.one()` (or their `scalar_` forms) on SQLAlchemy, the
/// first two of `.fetchmany(2)` on a driver.
pub(super) fn result_follows_sqlc(
    request: &plugin::GenerateRequest,
    query: &plugin::Query,
    function: &StmtFunctionDef,
    (body, models): (&[Stmt], &[Stmt]),
    (config, engine): (&Config, Engine),
    problems: &mut Vec<String>,
) {
    // A `:one` that always finds a row raises if it does not, whatever the options say.
    let backend = config.backend;
    let raises = config.raise_on_none || engine.single_row(&query.text);
    let Some(returns) = function.returns.as_deref() else {
        return problems.push(format!("{}: no return annotation", query.name));
    };

    // A scalar subquery that may find no row, an aggregate of no rows, and a column of a table an outer join left
    // without a row are NULL though sqlc reports them NOT NULL, and so may every column of a list we cannot read. An
    // embed is one column here, but each of its table's columns in the statement's list.
    let width = |column: &plugin::Column| {
        let embed = column.embed_table.as_ref().filter(|_| is_embed(column));
        embed.map_or(1, |embed| table(request, embed).map_or(0, |t| t.columns.len()))
    };
    let tables: Vec<&str> = (query.columns.iter())
        .flat_map(|column| match is_embed(column) {
            true => vec![""; width(column)],
            false => vec![column.table.as_ref().map_or("", |table| &table.name)],
        })
        .collect();
    let items = engine.items(&query.text, &tables);
    let columns: Vec<plugin::Column> = (query.columns.iter().enumerate())
        .map(|(i, column)| {
            let at: usize = query.columns[..i].iter().map(width).sum();
            let nullable = match &items {
                Some(items) => !is_embed(column) && items[at].nullable,
                None => true,
            };
            plugin::Column {
                not_null: column.not_null && !nullable,
                ..column.clone()
            }
        })
        .collect();
    let shape = |i: usize| Shape::from(&columns[i]);

    // A batch returns a list, its command's result per row.
    let (cmd, returns) = match (query.cmd.strip_prefix(":batch"), returns) {
        (Some(cmd @ ("one" | "many")), Expr::Subscript(list)) if is_name(&list.value, "list") => {
            (format!(":{cmd}"), &*list.slice)
        }
        _ => (query.cmd.clone(), returns),
    };
    let item = match (cmd.as_str(), returns) {
        (":one", _) => {
            let reads: &[&str] = match (backend, raises) {
                (Backend::SqlAlchemy, true) => &["one", "scalar_one"],
                (Backend::SqlAlchemy, false) => &["one_or_none", "scalar_one_or_none"],
                _ => &["fetchmany"],
            };
            if !calls_method(&function.body, reads) {
                problems.push(format!("{}: :one reads its row with none of {reads:?}", query.name));
            }
            // Without a row a `:one` returns `None`, so a record is `X | None`.
            match strip_optional(returns) {
                record if !raises && record != returns && self::record(record, body, models).is_some() => record,
                _ => returns,
            }
        }
        (":many", Expr::Subscript(list)) if is_name(&list.value, "list") => &*list.slice,
        (":many", _) => {
            return problems.push(format!("{}: {} returns `{}`", query.name, query.cmd, unparse(returns)));
        }
        // How many rows it wrote, as sqlc's Go returns
        (":copyfrom", returns) if !is_name(returns, "int") => {
            return problems.push(format!("{}: :copyfrom returns `{}`", query.name, unparse(returns)));
        }
        _ => return,
    };

    let disagrees = |what: &str| format!("{}: {what} disagrees with sqlc on NULL or array depth", query.name);
    match record(item, body, models) {
        Some((class, fields)) => {
            let model = item.is_attribute_expr();
            if model != reuses_model(request, &columns) {
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
                    .map(|((_, annotation), column)| match strip_optional(annotation) {
                        Expr::Attribute(model) if is_embed(column) => model.attr.as_str(),
                        _ => column.name.as_str(),
                    })
                    .collect();
                let what = format!("{}: class `{}`", query.name, class.name);
                named_after(
                    &what,
                    &fields,
                    &spellings,
                    Repeats::Numbered,
                    (Binding::Field, config.records),
                    problems,
                );
                let comments = query.columns.iter().map(|c| c.comment.as_str());
                documented(&what, class, "", comments, problems);
            }
            for (i, ((name, annotation), column)) in fields.into_iter().zip(&query.columns).enumerate() {
                if !is_embed(column) && Shape::of(annotation) != shape(i) {
                    problems.push(disagrees(&format!("field `{name}`")));
                }
            }
        }
        // A bare `:one` result is `None` without a row, unless it raises.
        None => match &query.columns[..] {
            [column] if cmd == ":one" && !raises => {
                let optional = Shape::from(&plugin::Column {
                    not_null: false,
                    ..column.clone()
                });
                if Shape::of(item) != optional {
                    problems.push(disagrees("the bare result"));
                }
            }
            [_] if Shape::of(item) == shape(0) => {}
            _ => problems.push(disagrees("the bare result")),
        },
    }
}

/// Whether `body` calls a method named one of `names`, like `x.one()`.
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

/// The catalog table an identifier names, in the default schema if it names none.
fn table<'a>(request: &'a plugin::GenerateRequest, id: &plugin::Identifier) -> Option<&'a plugin::Table> {
    let catalog = request.catalog.as_ref()?;
    let schema = match id.schema.as_str() {
        "" => catalog.default_schema.as_str(),
        schema => schema,
    };
    (catalog.schemas.iter().filter(|s| s.name == schema))
        .flat_map(|s| &s.tables)
        .find(|t| t.rel.as_ref().is_some_and(|rel| rel.name == id.name))
}

/// Whether a result is exactly one table's columns, so that its rows are that table's model as sqlc-gen-go reuses
/// one: at least two columns, each of that table, with its columns' names, order, types, nullability and array depth.
fn reuses_model(request: &plugin::GenerateRequest, columns: &[plugin::Column]) -> bool {
    let table_of = |column: &plugin::Column| column.table.as_ref().and_then(|t| table(request, t));
    let Some(model) = columns.first().and_then(table_of) else {
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
            .all(|c| table_of(c).is_some_and(|t| std::ptr::eq(t, model)))
        && columns.iter().zip(&model.columns).all(|(a, b)| same(a, b))
}

/// Whether a result column is `sqlc.embed(t)`.
fn is_embed(column: &plugin::Column) -> bool {
    (column.embed_table.as_ref()).is_some_and(|table| !table.name.is_empty())
}
