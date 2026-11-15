use std::collections::BTreeSet;

use ruff_python_ast::visitor::{Visitor, walk_expr};
use ruff_python_ast::{Expr, Stmt, StmtClassDef};
use sqlc_gen_python::api::names::{self, Repeats};

use super::python::{docstring, fields, is_name, unparse, words};

/// A record's `from_row` builds its fields in order, each from its own place in the row: a column from `row[i]`
/// (inside an enum's conversion, if any), and an embed of `width` columns from `row[a:b]`, starting where the field
/// before it ended. `widths` holds `None` for a column, and an outer join's embed may be `None` as one of its own
/// columns tells.
pub(super) fn reads_in_order(class: &StmtClassDef, widths: &[Option<usize>], problems: &mut Vec<String>) {
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
        // Any of an embed's own columns may tell whether an outer join found its row.
        let own_columns: BTreeSet<String> = match width {
            None => BTreeSet::new(),
            Some(width) => (start - width..start).map(|i| i.to_string()).collect(),
        };
        let extra: BTreeSet<&String> = reads.0.difference(&own_columns).collect();
        if !reads.0.contains(&expected) || extra != BTreeSet::from([&expected]) {
            problems.push(format!(
                "class `{}`: `from_row` reads row[{}] into `{}`, not row[{expected}]",
                class.name,
                reads.0.into_iter().collect::<Vec<_>>().join("], row["),
                keyword.arg.as_ref().map_or("?", |arg| arg.as_str()),
            ));
        }
    }
}

/// `fields` are named as the generator names `spellings` (`docs/python-output.md` §3), in order. We need this as two
/// fields of one type, swapped in the SQL, the signature and `from_row` at once, pass every other check and even run.
pub(super) fn named_after(
    what: &str,
    fields: &[(&str, &Expr)],
    spellings: &[&str],
    repeats: Repeats,
    problems: &mut Vec<String>,
) {
    let found: Vec<&str> = fields.iter().map(|(name, _)| *name).collect();
    let expected = names::names(spellings, repeats).unwrap_or_default();
    if found != expected {
        problems.push(format!("{what}: {found:?} are not named after sqlc's {spellings:?}"));
    }
}

/// A record's docstrings are exactly its comments (§4, rule 5): the class's `comment`, then each field's own. We only
/// compare the words, as the layout is for the golden tests to pin.
pub(super) fn documented<'a>(
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
    let found: Vec<String> = std::iter::once(docstring(body.first())).chain(attributes).collect();
    if found.iter().map(|d| words(d)).collect::<Vec<_>>() != expected {
        problems.push(format!("{what}: docstrings {found:?} are not its comments"));
    }
}
