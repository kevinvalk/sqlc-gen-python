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
///
/// TODO: We do not read a row compared with `IN`, `IS DISTINCT FROM` or `BETWEEN`, nor one in parentheses of its
/// own, where sqlc types each parameter by the first column just the same.
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

const COMPARISONS: &[&str] = &["=", "<>", "!=", "<", "<=", ">", ">="];

/// What ends the operand of a comparison, besides another comparison and what closes or divides its parentheses.
const BOUNDS: &[&str] = &[
    "and",
    "between",
    "by",
    "else",
    "end",
    "except",
    "fetch",
    "for",
    "from",
    "group",
    "having",
    "ilike",
    "in",
    "intersect",
    "is",
    "like",
    "limit",
    "not",
    "offset",
    "on",
    "or",
    "order",
    "returning",
    "select",
    "set",
    "then",
    "union",
    "when",
    "where",
    "window",
];

fn bounds(word: &str) -> bool {
    COMPARISONS.contains(&word) || is(word, BOUNDS)
}

/// The operand that `words` end with.
fn operand_before(words: &[String]) -> &[String] {
    let mut depth = 0usize;
    let start = (0..words.len()).rev().find(|&at| match words[at].as_str() {
        ")" | "]" => {
            depth += 1;
            false
        }
        "(" | "[" if depth > 0 => {
            depth -= 1;
            false
        }
        "(" | "[" | "," | ";" => true,
        word => depth == 0 && bounds(word),
    });
    &words[start.map_or(0, |at| at + 1)..]
}

/// The operand that `words` start with.
fn operand_after(words: &[String]) -> &[String] {
    let mut depth = 0usize;
    let end = (0..words.len()).find(|&at| match words[at].as_str() {
        "(" | "[" => {
            depth += 1;
            false
        }
        ")" | "]" if depth > 0 => {
            depth -= 1;
            false
        }
        ")" | "]" | "," | ";" => true,
        word => depth == 0 && bounds(word),
    });
    &words[..end.unwrap_or(words.len())]
}

/// The bracket that `words[at]` stands directly inside, if it does.
fn bracket(words: &[String], at: usize) -> Option<usize> {
    let mut depth = 0usize;
    (0..at).rev().find_map(|open| match words[open].as_str() {
        ")" | "]" => {
            depth += 1;
            None
        }
        "(" | "[" if depth > 0 => {
            depth -= 1;
            None
        }
        "[" => Some(Some(open)),
        "(" => Some(None),
        _ => None,
    })?
}

/// Whether `operand` ends with a subscript: the element of an array, where `ARRAY[...]` makes one.
fn subscripts(operand: &[String]) -> bool {
    operand.last().is_some_and(|close| close == "]")
        && bracket(operand, operand.len() - 1).is_some_and(|open| !(open > 0 && is(&operand[open - 1], &["array"])))
}

/// Whether each parenthesis and bracket that `words` close is one they opened, and none is left open.
fn balanced(words: &[String]) -> bool {
    let mut depth = 0i32;
    words.iter().all(|word| {
        depth += match word.as_str() {
            "(" | "[" => 1,
            ")" | "]" => -1,
            _ => 0,
        };
        depth >= 0
    }) && depth == 0
}

/// `operand` without the parentheses around the whole of it.
fn bare(mut operand: &[String]) -> &[String] {
    while let [open, inner @ .., close] = operand
        && open == "("
        && close == ")"
        && balanced(inner)
    {
        operand = inner;
    }
    operand
}

/// The type the whole of `operand` is cast to, if it is: `x::type` or `CAST(x AS type)`, without the type's modifiers.
fn cast_type(operand: &[String]) -> Option<String> {
    let ty = match bare(operand) {
        [cast, open, inner @ .., close] if is(cast, &["cast"]) && open == "(" && close == ")" => {
            &inner[inner.iter().rposition(|word| is(word, &["as"]))? + 1..]
        }
        operand => &operand[operand.windows(2).rposition(|pair| pair[0] == ":" && pair[1] == ":")? + 2..],
    };
    let name: Vec<&str> = (ty.iter().map(String::as_str))
        .take_while(|word| !matches!(*word, "(" | "["))
        .collect();
    let names = |word: &&str| *word == "." || Engine::PostgreSql.is_identifier(word);
    (!name.is_empty() && name.iter().all(names)).then(|| name.join(" ").replace(" . ", ".").to_ascii_lowercase())
}

/// Where the whole of `operand` is the call of a function, whether its result has the type of what it is given, as
/// only then sqlc's type by a column in there is the result's: `max(temp_lo)`, or `lower(name)` of text (of a range it
/// is the range's bound).
fn call_keeps_type(operand: &[String], ty: &str) -> Option<bool> {
    const KEEPS_TYPE: &[&str] = &["abs", "coalesce", "greatest", "least", "max", "min", "nullif"];
    const KEEPS_TEXT: &[&str] = &["btrim", "initcap", "lower", "ltrim", "rtrim", "trim", "upper"];
    let [name, open, arguments @ .., close] = bare(operand) else {
        return None;
    };
    if open != "(" || close != ")" || !balanced(arguments) || is(name, &["cast"]) {
        return None;
    }
    let is_text = matches!(ty, "text" | "varchar" | "bpchar" | "char" | "citext" | "name");
    let keeps = is(name, KEEPS_TYPE) || (is_text && is(name, KEEPS_TEXT));
    // What a call among its arguments returns is of another type again
    let plain = !arguments.iter().any(|word| word == "(");
    Engine::PostgreSql.is_identifier(name).then_some(keeps && plain)
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
        let comparison = |op: &str| COMPARISONS.contains(&op);
        let is_json = matches!(ty.as_str(), "json" | "jsonb");
        // What the parameter is held against: sqlc types it by the first column in there
        let other = match (before, after) {
            ([compared @ .., op], _) if comparison(op) || is(op, &["like", "ilike"]) => operand_before(compared),
            ([compared @ .., r#in, open], _) if open == "(" && is(r#in, &["in"]) => operand_before(compared),
            (_, [op, compared @ ..]) if comparison(op) => operand_after(compared),
            _ => &[],
        };
        let quantifies = |words: &[String]| matches!(words, [quantifier, open, ..] if open == "(" && is(quantifier, &["any", "all", "some"]));
        let inside = bracket(words, at);
        let is_time = matches!(ty.as_str(), "date" | "timestamp" | "timestamptz" | "time" | "timetz");
        let adds = |operand: &[String]| operand.iter().any(|word| matches!(word.as_str(), "+" | "-"));
        // PostgreSQL can subtract two dates or timestamps. A later comparison can also coerce a parameter
        // whose type PostgreSQL already inferred at its first use.
        let reason = if column.is_array && inside.is_some_and(|open| open > 0 && is(&words[open - 1], &["array"])) {
            "an ARRAY[...] takes the elements of the array"
        } else if column.is_array && inside.is_some() {
            "an array subscript takes an integer"
        } else if is_json && matches!(previous, "?" | "?|" | "?&" | "->" | "->>" | "#>" | "#>>") {
            "a JSON lookup takes a key, index or path"
        } else if is_json && matches!(previous, "-" | "#-" | "@?" | "@@") {
            "this JSON operator takes a key, a path or a jsonpath"
        } else if words[..at].contains(token) {
            continue;
        } else if !column.is_array && before.len() >= 2 && quantifies(&before[before.len() - 2..]) {
            "ANY, ALL and SOME take an array"
        } else if column.is_array && matches!(after, [op, quantified @ ..] if comparison(op) && quantifies(quantified))
        {
            "ANY, ALL and SOME compare it with each element of the array"
        } else if column.is_array && subscripts(other) {
            "the other operand is one element of the array"
        } else if is_json && other.iter().any(|word| matches!(word.as_str(), "->>" | "#>>")) {
            "the ->> and #>> operators give text"
        } else if previous == "-" && ty == "timetz" {
            "subtracting from a time with time zone takes an interval"
        } else if is_time && (previous == "+" || next == "+") {
            "date and time addition needs a different operand type"
        } else if is_time && adds(other) {
            "date and time arithmetic gives another type"
        } else if cast_type(other).is_some_and(|cast| Engine::PostgreSql.type_name(&cast) != ty) {
            "the comparison casts the column to another type"
        } else if column.table.is_some() && call_keeps_type(other, &ty) == Some(false) {
            "the other operand is a function's result"
        } else {
            continue;
        };
        let typed = match column.is_array {
            true => format!("{ty}[]"),
            false => ty,
        };
        bail!(Problem::new(
            format!("parameter {token} was typed by its column as {typed}, but {reason}"),
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
