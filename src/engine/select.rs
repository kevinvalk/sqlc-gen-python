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

/// What each of `columns` result columns is, by position, from the
/// statement's code `tokens`; `None` where not certain.
pub fn items(tokens: &[String], columns: usize) -> Option<Vec<Item>> {
    let is = |token: &String, words: &[&str]| {
        (words.iter()).any(|word| token.eq_ignore_ascii_case(word))
    };
    // Each token with its depth before it.
    let mut depth = 0usize;
    let mut leveled = Vec::new();
    for token in tokens {
        if token == ")" {
            depth = depth.checked_sub(1)?;
        }
        leveled.push((depth, token));
        if token == "(" {
            depth += 1;
        }
    }
    let top = |words: &[&str]| leveled.iter().position(|&(d, t)| d == 0 && is(t, words));
    if top(&["union", "intersect", "except"]).is_some() {
        return None;
    }
    let start = top(&["returning"]).or_else(|| top(&["select"]))? + 1;
    const ENDS: &[&str] = &[
        "from", "where", "group", "having", "window", "order", "limit", "offset", "fetch", "for",
        "into", ";",
    ];
    let mut rest = &leveled[start..];
    // `DISTINCT`, `DISTINCT ON (...)` or `ALL` before the list.
    if let [(_, first), more @ ..] = rest
        && is(first, &["distinct", "all"])
    {
        rest = more;
        if let [(_, on), more @ ..] = rest
            && is(on, &["on"])
        {
            let close = more.iter().position(|&(d, t)| d == 0 && t == ")")?;
            rest = &more[close + 1..];
        }
    }
    let end = rest.iter().position(|&(d, t)| d == 0 && is(t, ENDS));
    let list = &rest[..end.unwrap_or(rest.len())];
    let items: Vec<&[(usize, &String)]> = list.split(|&(d, t)| d == 0 && t == ",").collect();
    if items.len() != columns {
        return None;
    }
    Some(items.into_iter().map(item).collect())
}

/// Whether the statement returns exactly one row, whatever the data: an
/// `INSERT` of one row of `VALUES` (or `DEFAULT VALUES`) `RETURNING` it, not
/// `ON CONFLICT DO NOTHING`; or a `SELECT` whose every item is an aggregate
/// (`count`, `sum`, ... and `EXISTS`), or a `coalesce` or `CAST` of one,
/// without a top-level `GROUP BY`, `HAVING`, `LIMIT`, `OFFSET`, `FETCH`,
/// window or set operation.
pub fn single_row(tokens: &[String]) -> bool {
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
    const WRAPPERS: &[&str] = &["coalesce", "ifnull", "cast"];
    let is = |token: &str, words: &[&str]| words.iter().any(|w| token.eq_ignore_ascii_case(w));
    let mut depth = 0usize;
    let mut top = Vec::new();
    for token in tokens {
        if token == ")" {
            depth = depth.saturating_sub(1);
        }
        top.push((depth, token.as_str()));
        if token == "(" {
            depth += 1;
        }
    }
    let at_top = |words: &[&str]| top.iter().any(|&(d, t)| d == 0 && is(t, words));
    if top.first().is_some_and(|&(_, t)| is(t, &["insert"])) {
        let Some(values) = top.iter().position(|&(d, t)| d == 0 && is(t, &["values"])) else {
            return false;
        };
        let after = &top[values + 1..];
        let end = (after
            .iter()
            .position(|&(d, t)| d == 0 && is(t, &["returning", "on"])))
        .unwrap_or(after.len());
        let defaults = values > 0 && is(top[values - 1].1, &["default"]);
        let rows = after[..end]
            .iter()
            .filter(|&&(d, t)| d == 0 && t == "(")
            .count();
        return at_top(&["returning"])
            && !at_top(&["select", "nothing"])
            && (rows == 1 || (defaults && rows == 0));
    }
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
    let Some(select) = top.iter().position(|&(d, t)| d == 0 && is(t, &["select"])) else {
        return false;
    };
    if at_top(ROWS) {
        return false;
    }
    let end = (top[select + 1..]
        .iter()
        .position(|&(d, t)| d == 0 && is(t, &["from", "where", ";"])))
    .map_or(top.len(), |at| select + 1 + at);
    let list = &top[select + 1..end];
    // Each item: an aggregate's call, or a wrapper's around one.
    list.split(|&(d, t)| d == 0 && t == ",")
        .all(|item| match item {
            [(_, name), (_, "("), ..] if is(name, AGGREGATES) => true,
            [(_, name), (_, "("), (_, inner), (_, "("), ..] => {
                is(name, WRAPPERS) && is(inner, AGGREGATES)
            }
            _ => false,
        })
        && !list.is_empty()
}

/// The tables the statement's own outer joins may leave without a row, by
/// the name each is joined as, lowercase: each one after a `LEFT JOIN`. A
/// `RIGHT` or `FULL` join may leave any of them without one: `None`.
pub fn outer_joined(tokens: &[String]) -> Option<Vec<String>> {
    let mut depth = 0usize;
    let mut joined = Vec::new();
    let top: Vec<&String> = (tokens.iter())
        .filter(|token| {
            let level = depth;
            match token.as_str() {
                "(" => depth += 1,
                ")" => depth = depth.saturating_sub(1),
                _ => {}
            }
            level == 0 && !matches!(token.as_str(), "(" | ")")
        })
        .collect();
    for (i, token) in top.iter().enumerate() {
        if !token.eq_ignore_ascii_case("join") {
            continue;
        }
        let side = (top[..i].iter().rev())
            .find(|t| !t.eq_ignore_ascii_case("outer"))
            .map(|t| t.to_lowercase());
        match side.as_deref() {
            Some("right" | "full") => return None,
            Some("left") => {}
            _ => continue,
        }
        // `schema.table`: the table, unquoted.
        let mut name = top.get(i + 1).map(|t| t.as_str());
        let mut j = i + 1;
        while top.get(j + 1).is_some_and(|t| *t == ".") {
            j += 2;
            name = top.get(j).map(|t| t.as_str());
        }
        if let Some(name) = name {
            joined.push(name.trim_matches(['"', '`', '[', ']']).to_lowercase());
        }
    }
    Some(joined)
}

/// What `item`, of the list's top level, is.
fn item(item: &[(usize, &String)]) -> Item {
    match item {
        [(_, open), ..] if *open == "(" => match parenthesised(item) {
            Some(inner @ [(_, select), ..]) if select.eq_ignore_ascii_case("select") => {
                let inner: Vec<String> = inner.iter().map(|(_, t)| (*t).clone()).collect();
                let grouped = inner.iter().any(|t| t.eq_ignore_ascii_case("group"));
                let counts = items(&inner, 1) == Some(vec![Item::Call("count".into())]);
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
fn parenthesised<'t>(tokens: &'t [(usize, &'t String)]) -> Option<&'t [(usize, &'t String)]> {
    let [(depth, open), rest @ ..] = tokens else {
        return None;
    };
    let close = rest.iter().position(|&(d, t)| d == *depth && t == ")")?;
    let alias = match &rest[close + 1..] {
        [] => true,
        [(_, alias)] => is_word(alias),
        [(_, r#as), (_, alias)] => r#as.eq_ignore_ascii_case("as") && is_word(alias),
        _ => false,
    };
    (*open == "(" && alias).then_some(&rest[..close])
}

/// Whether `token` is a word, quoted or not: a name or an alias.
fn is_word(token: &str) -> bool {
    token.starts_with(|c: char| c.is_alphabetic() || "_\"`[".contains(c))
}
