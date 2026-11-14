//! What each result column of a statement is, read off its tokens, where
//! sqlc's report is not enough: it types a call by the function's first
//! overload without saying which function it called, and reports a scalar
//! subquery NOT NULL, though it is NULL when it finds no row.
//!
//! Only what is certain is read. The result list is the top-level `SELECT`'s
//! (after any `WITH`) or the `RETURNING` list, split at its top-level commas.
//! An item is a call when it is a bare name and its parenthesised arguments,
//! and a subquery when it is a parenthesised `SELECT`, either with at most an
//! alias after it. A statement with a top-level `UNION`, `INTERSECT` or
//! `EXCEPT` (whose types the branches decide together), or whose list does
//! not have one item per column, has no answer.
//!
//! sqlc reports every `sqlc.embed` nullable, joined or not, so which tables
//! an outer join may leave without a row is read here too.

#[cfg(test)]
#[path = "select_spec.rs"]
mod spec;

/// A result column's item of the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// `f(...)`: a bare call of `f`, lowercase.
    Call(String),
    /// `(SELECT ...)`: a scalar subquery, which always has a `row` only if it
    /// counts, ungrouped.
    Subquery { row: bool },
    /// Anything else.
    Other,
}

/// A token of a statement's code, with the depth of parentheses it stands at.
type Leveled<'t> = (usize, &'t str);

/// Each of `tokens` with its depth: a parenthesis stands at the depth it
/// opens from, or closes to.
fn leveled(tokens: &[String]) -> Vec<Leveled<'_>> {
    let mut depth = 0usize;
    (tokens.iter())
        .map(|token| {
            if token == ")" {
                depth = depth.saturating_sub(1);
            }
            let level = depth;
            if token == "(" {
                depth += 1;
            }
            (level, token.as_str())
        })
        .collect()
}

/// Whether `token` is one of `words`, in any case.
fn is(token: &str, words: &[&str]) -> bool {
    words.iter().any(|word| token.eq_ignore_ascii_case(word))
}

/// Where the first of `words` stands at the top level of `tokens`.
fn top(tokens: &[Leveled], words: &[&str]) -> Option<usize> {
    tokens.iter().position(|&(d, t)| d == 0 && is(t, words))
}

/// What each of `columns` result columns is, by position, from the
/// statement's code `tokens`; `None` where not certain.
pub fn items(tokens: &[String], columns: usize) -> Option<Vec<Item>> {
    list(&leveled(tokens), columns)
}

/// [`items`], of leveled tokens.
fn list(tokens: &[Leveled], columns: usize) -> Option<Vec<Item>> {
    const ENDS: &[&str] = &[
        "from", "where", "group", "having", "window", "order", "limit", "offset", "fetch", "for",
        "into", ";",
    ];
    if top(tokens, &["union", "intersect", "except"]).is_some() {
        return None;
    }
    let start = top(tokens, &["returning"]).or_else(|| top(tokens, &["select"]))? + 1;
    let mut rest = &tokens[start..];

    // `DISTINCT`, `DISTINCT ON (...)` or `ALL` before the list.
    if let [(_, first), more @ ..] = rest
        && is(first, &["distinct", "all"])
    {
        rest = more;
        if let [(_, on), more @ ..] = rest
            && is(on, &["on"])
        {
            rest = &more[top(more, &[")"])? + 1..];
        }
    }

    let list = &rest[..top(rest, ENDS).unwrap_or(rest.len())];
    let items: Vec<&[Leveled]> = list.split(|&(d, t)| d == 0 && t == ",").collect();
    (items.len() == columns).then(|| items.into_iter().map(item).collect())
}

/// Whether the statement returns exactly one row, whatever the data: an
/// `INSERT` of one row of `VALUES` (or `DEFAULT VALUES`) `RETURNING` it,
/// which no `OR IGNORE`, `ON CONFLICT DO NOTHING` or `DO UPDATE ... WHERE`
/// can skip; or a `SELECT` whose every item is an aggregate (`count`, `sum`,
/// ... and `EXISTS`), or a `coalesce` or `CAST` of one, without a top-level
/// `GROUP BY`, `HAVING`, `LIMIT`, `OFFSET`, `FETCH`, window or set operation.
pub fn single_row(tokens: &[String]) -> bool {
    const WRAPPERS: &[&str] = &["coalesce", "ifnull", "cast"];
    const ROWS: &[&str] = &[
        "group",
        "having",
        "limit",
        "offset",
        "fetch",
        "over",
        "window",
        "union",
        "intersect",
        "except",
        "returning",
        "insert",
        "update",
        "delete",
    ];
    let tokens = leveled(tokens);
    if tokens.first().is_some_and(|&(_, t)| is(t, &["insert"])) {
        return inserts_one(&tokens);
    }
    let Some(select) = top(&tokens, &["select"]) else {
        return false;
    };
    if top(&tokens, ROWS).is_some() {
        return false;
    }
    let list = &tokens[select + 1..];
    let list = &list[..top(list, &["from", "where", ";"]).unwrap_or(list.len())];
    !list.is_empty()
        && list
            .split(|&(d, t)| d == 0 && t == ",")
            .all(|item| match item {
                [(_, wrapper), (_, "("), inner @ ..] if is(wrapper, WRAPPERS) => aggregates(inner),
                item => aggregates(item),
            })
}

/// Whether `item` opens with an aggregate's call. SQLite's `min` and `max`
/// of several arguments are scalar functions, of each row.
fn aggregates(item: &[Leveled]) -> bool {
    const AGGREGATES: &[&str] = &[
        "count",
        "sum",
        "avg",
        "min",
        "max",
        "total",
        "every",
        "bool_and",
        "bool_or",
        "string_agg",
        "array_agg",
        "json_agg",
        "jsonb_agg",
        "group_concat",
        "exists",
    ];
    let [(depth, name), (_, "("), arguments @ ..] = item else {
        return false;
    };
    let several = (arguments.iter())
        .take_while(|&&(d, t)| !(d == *depth && t == ")"))
        .any(|&(d, t)| d == depth + 1 && t == ",");
    is(name, AGGREGATES) && !(several && is(name, &["min", "max"]))
}

/// Whether an `INSERT` returns the one row it inserts.
fn inserts_one(tokens: &[Leveled]) -> bool {
    let (Some(values), Some(returning)) = (top(tokens, &["values"]), top(tokens, &["returning"]))
    else {
        return false;
    };
    let rows = (tokens[values..returning].iter())
        .take_while(|&&(d, t)| !(d == 0 && is(t, &["on"])))
        .filter(|&&(d, t)| d == 0 && t == "(")
        .count();
    let defaults = values > 0 && is(tokens[values - 1].1, &["default"]);

    // What may leave the row out: SQLite's `INSERT OR IGNORE`, `DO NOTHING`,
    // and the `WHERE` of a `DO UPDATE`.
    let ignores = matches!(tokens, [_, (_, or), (_, ignore), ..] if is(or, &["or"]) && is(ignore, &["ignore"]));
    let skips = top(tokens, &["do"])
        .is_some_and(|action| top(&tokens[action..returning], &["nothing", "where"]).is_some());
    (rows == 1 || (defaults && rows == 0))
        && top(tokens, &["select"]).is_none()
        && !ignores
        && !skips
}

/// The tables the statement's own outer joins may leave without a row, by
/// the name each is joined as, lowercase: each one after a `LEFT JOIN`. `None`
/// for any of them: after a `RIGHT` or `FULL` join, or where an outer join is
/// of, or within, a parenthesised join, whose tables are not read here. A
/// subquery's joins are its own.
pub fn outer_joined(tokens: &[String]) -> Option<Vec<String>> {
    const SUBQUERY: &[&str] = &["select", "with", "values"];
    let tokens = leveled(tokens);
    // Whether each open parenthesis is a subquery's.
    let mut subqueries: Vec<bool> = Vec::new();
    let mut joined = Vec::new();
    for (i, &(depth, token)) in tokens.iter().enumerate() {
        let next = tokens.get(i + 1).map_or("", |&(_, t)| t);
        match token {
            "(" => subqueries.push(is(next, SUBQUERY)),
            ")" => {
                subqueries.pop();
            }
            _ => {}
        }
        if !is(token, &["join"]) || subqueries.contains(&true) {
            continue;
        }
        let side = (tokens[..i].iter().rev())
            .map(|&(_, t)| t)
            .find(|t| !is(t, &["outer"]));
        match side {
            Some(side) if is(side, &["right", "full"]) => return None,
            Some(side) if is(side, &["left"]) => {}
            _ => continue,
        }
        let after: Vec<&str> = (tokens[i + 1..].iter())
            .map(|&(_, t)| t)
            .skip_while(|t| is(t, &["lateral", "only"]))
            .collect();
        let mut name = match after.as_slice() {
            // A subquery is no table, whatever its alias.
            ["(", first, ..] if is(first, SUBQUERY) => continue,
            ["(", ..] => return None,
            _ if depth > 0 => return None,
            name => name.iter().peekable(),
        };

        // `schema.table`: the table, unquoted.
        let mut table = name.next();
        while name.next_if_eq(&&".").is_some() {
            table = name.next();
        }
        if let Some(table) = table {
            joined.push(table.trim_matches(['"', '`', '[', ']']).to_lowercase());
        }
    }
    Some(joined)
}

/// What `item`, of the list's top level, is.
fn item(item: &[Leveled]) -> Item {
    match item {
        [(_, "("), ..] => match parenthesised(item) {
            Some(inner @ [(depth, select), ..]) if is(select, &["select"]) => {
                // The subquery's own tokens, leveled from its own top.
                let inner: Vec<Leveled> = inner.iter().map(|&(d, t)| (d - depth, t)).collect();
                let grouped = inner.iter().any(|&(_, t)| is(t, &["group"]));
                let counts = list(&inner, 1) == Some(vec![Item::Call("count".into())]);
                Item::Subquery {
                    row: counts && !grouped,
                }
            }
            _ => Item::Other,
        },
        [(_, name), rest @ ..] if is_word(name) && !name.starts_with(['"', '`', '[']) => {
            match parenthesised(rest) {
                Some(_) => Item::Call(name.to_lowercase()),
                None => Item::Other,
            }
        }
        _ => Item::Other,
    }
}

/// What `tokens` hold in parentheses, if they are those parentheses and at
/// most an alias after them.
fn parenthesised<'t>(tokens: &'t [Leveled<'t>]) -> Option<&'t [Leveled<'t>]> {
    let [(depth, "("), rest @ ..] = tokens else {
        return None;
    };
    let close = rest.iter().position(|&(d, t)| d == *depth && t == ")")?;
    let alias = match &rest[close + 1..] {
        [] => true,
        [(_, alias)] => is_word(alias),
        [(_, r#as), (_, alias)] => is(r#as, &["as"]) && is_word(alias),
        _ => false,
    };
    alias.then_some(&rest[..close])
}

/// Whether `token` is a word, quoted or not: a name or an alias.
fn is_word(token: &str) -> bool {
    token.starts_with(|c: char| c.is_alphabetic() || "_\"`[".contains(c))
}
