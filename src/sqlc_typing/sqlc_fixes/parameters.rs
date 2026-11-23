#[cfg(test)]
#[path = "parameters_spec.rs"]
mod spec;

use std::collections::HashMap;

use anyhow::{Result, bail};

use super::types;
use crate::engine::Engine;
use crate::plugin;
use crate::problem::Problem;
use crate::sqlc_typing::tokens::{Leveled, column, is, is_word, leveled, parenthesised};

/// sqlc's PostgreSQL analyser takes a parameter's type from the first column it finds in a comparison,
/// even in a row of several columns (`internal/compiler/resolve.go`, `ast.A_Expr`).
fn check_parameters(engine: Engine, query: &plugin::Query) -> Result<()> {
    if engine == Engine::PostgreSql && !query.params.is_empty() {
        let words = engine.words(&query.text);
        check_operands(&words, &query.params)?;
        for (number, field) in row_parameters(&words) {
            if let Some(column) = query
                .params
                .iter()
                .find(|p| p.number == number)
                .and_then(|p| p.column.as_ref())
                && column.table.is_some()
                && !column.original_name.is_empty()
                && column.original_name != field
            {
                bail!(Problem::new(
                    format!(
                        "parameter ${number} is compared with \"{field}\" in a row, but sqlc typed it by \"{}\"",
                        column.original_name
                    ),
                    format!("cast the parameter to the type of \"{field}\"")
                ));
            }
        }
    }
    Ok(())
}

/// Plain placeholders compared positionally to columns in a PostgreSQL row comparison. A cast on the
/// placeholder gives sqlc its own type, so we leave those out.
fn row_parameters(words: &[String]) -> Vec<(i32, String)> {
    let tokens = leveled(words);
    let mut parameters = Vec::new();
    for (at, &(depth, op)) in tokens.iter().enumerate() {
        if !matches!(op, "=" | "<" | "<=" | ">" | ">=" | "<>" | "!=") || at == 0 || tokens[at - 1].1 != ")" {
            continue;
        }
        let right = match &tokens[at + 1..] {
            [(_, row), rest @ ..] if is(row, &["row"]) => rest,
            rest => rest,
        };
        let Some((right, _)) = parenthesised(right) else {
            continue;
        };
        let Some(open) = tokens[..at].iter().rposition(|t| *t == (depth, "(")) else {
            continue;
        };
        let before = open.checked_sub(1).map_or("", |i| tokens[i].1);
        if is_word(before)
            && !is(
                before,
                &["row", "where", "and", "or", "on", "when", "select", "not", "having"],
            )
        {
            continue;
        }
        let comma = |t: &Leveled| *t == (depth + 1, ",");
        let left = &tokens[open + 1..at - 1];
        if !left.iter().any(comma) || !right.iter().any(comma) {
            continue;
        }
        for (left, right) in left.split(comma).zip(right.split(comma)) {
            for (slot, field) in [(left, right), (right, left)] {
                if let [(_, placeholder)] = slot
                    && let Some(number) = placeholder.strip_prefix('$').and_then(|n| n.parse().ok())
                    && let Some(parts) = column(field)
                    && let Some(name) = parts.last()
                {
                    let name = match name.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
                        Some(quoted) => quoted.replace("\"\"", "\""),
                        None => name.to_ascii_lowercase(),
                    };
                    parameters.push((number, name));
                }
            }
        }
    }
    parameters
}

/// sqlc can give a placeholder the type of a column on the other side of an operator. We keep its binding
/// number and ask for a cast where that column's type cannot establish the operand's type.
fn check_operands(words: &[String], parameters: &[plugin::Parameter]) -> Result<()> {
    for (at, token) in words.iter().enumerate() {
        let Some(number) = token.strip_prefix('$').and_then(|number| number.parse::<i32>().ok()) else {
            continue;
        };
        let Some(column) = parameters
            .iter()
            .find(|p| p.number == number)
            .and_then(|p| p.column.as_ref())
        else {
            continue;
        };
        let (mut before, mut after) = (&words[..at], &words[at + 1..]);
        while let ([left @ .., open], [close, right @ ..]) = (before, after)
            && open == "("
            && close == ")"
            && left.last().is_none_or(|s| !Engine::PostgreSql.is_identifier(s))
        {
            (before, after) = (left, right);
        }
        if matches!(after, [a, b, ..] if a == ":" && b == ":") {
            continue;
        }
        let previous = before.last().map_or("", String::as_str);
        let next = after.first().map_or("", String::as_str);
        let ty = Engine::PostgreSql.type_name(&column.data_type());
        let comparison = |op: &str| matches!(op, "=" | "<>" | "!=" | "<" | "<=" | ">" | ">=");
        let cast = match (before, after) {
            ([.., a, b, name, op], _) if a == ":" && b == ":" && comparison(op) => Some(name),
            (_, [op, _, a, b, name, ..]) if comparison(op) && a == ":" && b == ":" => Some(name),
            ([.., close, op], _) if close == ")" && comparison(op) => {
                let mut depth = 0;
                before[..before.len() - 1]
                    .iter()
                    .rposition(|t| {
                        if t == ")" {
                            depth += 1;
                        }
                        if t == "(" {
                            depth -= 1;
                        }
                        depth == 0
                    })
                    .and_then(|at| match (&before[..at], &before[at + 1..before.len() - 2]) {
                        ([.., cast], [.., keyword, name])
                            if cast.eq_ignore_ascii_case("cast") && keyword.eq_ignore_ascii_case("as") =>
                        {
                            Some(name)
                        }
                        _ => None,
                    })
            }
            _ => None,
        };
        // PostgreSQL can subtract two dates or timestamps. A later comparison can also coerce a parameter
        // whose type PostgreSQL already inferred at its first use.
        let reason = if previous == "[" && next == "]" && column.is_array {
            "an array subscript takes an integer"
        } else if matches!(previous, "?" | "?|" | "?&" | "->" | "->>" | "#>" | "#>>")
            && matches!(ty.as_str(), "json" | "jsonb")
        {
            "a JSON lookup takes a key, index or path"
        } else if words[..at].contains(token) {
            continue;
        } else if previous == "-" && ty == "timetz" {
            "subtracting from a time with time zone takes an interval"
        } else if (previous == "+" || next == "+")
            && matches!(ty.as_str(), "date" | "timestamp" | "timestamptz" | "time" | "timetz")
        {
            "date and time addition needs a different operand type"
        } else if cast.is_some_and(|cast| Engine::PostgreSql.type_name(&cast.to_ascii_lowercase()) != ty) {
            "the comparison casts the column to another type"
        } else {
            continue;
        };
        bail!(Problem::new(
            format!("parameter {token} was typed by its column as {ty}, but {reason}"),
            "explicitly cast the parameter at its use"
        ));
    }
    Ok(())
}

/// sqlc's MySQL and SQLite requests bind positional placeholders in request order, even when their numbers differ.
pub(crate) fn bind_number(engine: Engine, position: usize, parameter: &plugin::Parameter) -> i32 {
    match engine {
        Engine::PostgreSql => parameter.number,
        Engine::MySql | Engine::Sqlite => position as i32 + 1,
    }
}

pub(crate) fn parameter_columns(engine: Engine, query: &plugin::Query) -> Result<Vec<plugin::Column>> {
    check_parameters(engine, query)?;
    let is_guess = |column: &plugin::Column| types::is_guess(Engine::MySql, column, true);
    let mut named: HashMap<&str, &plugin::Column> = HashMap::new();
    for column in query
        .params
        .iter()
        .filter_map(|p| p.column.as_ref())
        .filter(|c| c.is_named_param)
    {
        // PostgreSQL can parse `@day::date AT TIME ZONE 'UTC'` as `@(pg_catalog.timezone(...))`. sqlc can
        // replace the whole expression with a placeholder and give it the flattened function call as a name.
        if !engine.is_identifier(&column.name)
            || (engine == Engine::PostgreSql && column.name.starts_with("pg_catalog"))
        {
            bail!(Problem::new(
                format!(
                    "sqlc named a parameter \"{}\", which is no name: sqlc.arg was given one that is no identifier, \
                     or an operator that binds tighter than @, such as AT TIME ZONE or +, made @ take an expression, \
                     all of which sqlc may have replaced",
                    column.name
                ),
                "write sqlc.arg(name), or parenthesise (@name::type)"
            ));
        }
        // sqlc shares a placeholder across uses on PostgreSQL and SQLite. Only MySQL reports one per use.
        if engine != Engine::MySql {
            continue;
        }
        if let Some(other) = named.get(column.name.as_str()) {
            if other.is_sqlc_slice != column.is_sqlc_slice {
                bail!(Problem::new(
                    format!(
                        "parameter \"{}\" is used as both a scalar and a sqlc.slice",
                        column.name
                    ),
                    "give the scalar and slice different names"
                ));
            }
            if !is_guess(other) {
                continue;
            }
        }
        named.insert(&column.name, column);
    }

    // MySQL reports each use of a name separately. In `sqlc.narg(x) IS NULL OR col = sqlc.narg(x)` the
    // comparison supplies the missing type and its source column. Each occurrence keeps its own nullability.
    Ok(query
        .params
        .iter()
        .map(|parameter| {
            let Some(column) = &parameter.column else {
                return plugin::Column::default();
            };
            if column.is_named_param
                && let Some(typed) = named.get(column.name.as_str())
                && is_guess(column)
                && !is_guess(typed)
            {
                return plugin::Column {
                    r#type: typed.r#type.clone(),
                    table: typed.table.clone(),
                    original_name: typed.original_name.clone(),
                    is_func_call: typed.is_func_call,
                    is_array: typed.is_array,
                    array_dims: typed.array_dims,
                    length: typed.length,
                    unsigned: typed.unsigned,
                    ..column.clone()
                };
            }
            column.clone()
        })
        .collect())
}
