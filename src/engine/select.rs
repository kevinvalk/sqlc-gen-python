//! We read a statement's tokens where sqlc's report of its result is not enough. sqlc types a call by the function's
//! first overload without saying which function it called, and a `coalesce`, a `nullif` and a `CASE` by one operand.
//! It reports a scalar subquery and every cast NOT NULL, though a subquery without a row, an aggregate of no rows, a
//! `nullif` of equal arguments and a `CASE` where no branch holds are NULL. It reports every `sqlc.embed` nullable,
//! joined or not. And it reports NOT NULL the key a grouping set leaves out and the columns of a table an outer join
//! may leave without a row: left of a `RIGHT` or `FULL` join of several tables, and on SQLite and MySQL what a `LEFT`
//! join brings once a `JOIN` follows it.
//!
//! IMPORTANT: Each reader answers only what it can prove, and what it cannot read gives the answer that widens a type
//! (a column that may be NULL, a statement that may return no row, a table that may be missing). A wrong `X | None` is
//! a nuisance, a wrong `X` is a value that is not what its type says.
//!
//! We read a statement at its top level, the tokens outside every parenthesis and bracket. A set operation (`UNION`,
//! `INTERSECT`, `EXCEPT`) splits a `SELECT` into branches we read on their own, and a branch in parentheses as what it
//! holds. A word is only a keyword where it can be one: after a `.` or an `AS` it is a name, whatever it spells.

use std::ops::Range;

use super::Engine;

#[cfg(test)]
#[path = "select_spec.rs"]
mod spec;

/// The aggregate functions of an engine, each of which makes the rows it reads one row when nothing groups them.
pub struct Aggregates {
    /// NULL of no rows: `sum`, `max`, ...
    pub nullable: &'static [&'static str],
    /// Never NULL: `count`.
    pub never_null: &'static [&'static str],
    /// No aggregate when given several arguments, but a function of each row: SQLite's `min` and `max`.
    pub scalar_of_several: &'static [&'static str],
    pub engine: Engine,
}

/// One item of a result list: a column of the result, or one of an embed's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Item {
    /// Whether it is a bare `count(...)` (filtered or over a window as it may be), or a scalar subquery of one alone.
    pub count: bool,
    /// A decimal integer literal in the range every engine returns as an integer.
    pub integer: bool,
    /// PostgreSQL's array_agg preserves a NULL input as an element of the array.
    pub collects_nulls: bool,
    /// Whether it may be NULL where sqlc reports NOT NULL: a scalar subquery that may find no row, an aggregate of what
    /// may be no rows, a `CASE` that may take no branch or a `NULL` one, a `nullif`, the literal `NULL` itself, a key
    /// its grouping set leaves out, or a column of a source an outer join may leave without a row.
    pub nullable: bool,
    /// Whether it is a `coalesce`, a `nullif` or a `CASE` alone, in parentheses as it may be. sqlc types those by one
    /// operand whatever the others are: `coalesce(quantity, 0.5)` as `quantity`, `nullif(label, '')` as `bool`, and
    /// `CASE WHEN paid THEN paid_on ELSE '2020-01-01' END` as its `ELSE`, a `text`.
    pub operand_typed: bool,
}

/// What each item of the statement's result list is, by position, where `tables` holds the table sqlc reports for each
/// item's column, `schema.table` where it reports a schema (empty for none). The list is the top-level `SELECT`'s
/// (after any `WITH`) or the `RETURNING` one. In a set operation an item is a count only if it is one in every branch,
/// and nullable if it is in any. `None` when the list does not read as one item per column, as we then know nothing of
/// any of them.
pub fn items(
    tokens: &[String],
    tables: &[&str],
    aggregates: &Aggregates,
    required: impl Fn(&str, &str, &str) -> Option<bool>,
) -> Option<Vec<Item>> {
    let mut read: Option<Vec<Item>> = None;
    for branch in branches(&leveled(tokens)) {
        let list = &branch[list(&branch)?];
        let split: Vec<&[Leveled]> = list.split(|&(d, t)| d == 0 && t == ",").collect();
        // An empty item is a list cut short, as SQLite and MySQL allow a column named as a clause (`offset`)
        if split.iter().any(|item| item.is_empty()) || split.len() != tables.len() {
            return None;
        }
        let sources = sources_of(&branch, aggregates);
        let grouped = grouped(&branch);
        let required_column = |tokens: &[Leveled]| {
            let (ctes, sources) = sources.as_ref()?;
            column_required(tokens, ctes, sources, &required)
        };
        let bare = !grouped
            && (rolled(&branch)
                || (aggregates.engine != Engine::PostgreSql && aggregates_rows(&branch, list, aggregates)));
        let items = (split.iter().zip(tables)).map(|(tokens, table)| {
            let item = item(tokens, grouped, bare, aggregates, required_column);
            let outer = match &sources {
                Some((_, sources)) => outer(tokens, table, sources),
                // A `FROM` we could not read may leave any of its tables without a row
                None => column(tokens).is_some(),
            };
            Item {
                nullable: item.nullable || outer || cast_input(tokens).and_then(required_column) == Some(false),
                ..item
            }
        });
        read = Some(match read {
            None => items.collect(),
            Some(first) => (first.into_iter().zip(items))
                .map(|(a, b)| Item {
                    count: a.count && b.count,
                    integer: a.integer && b.integer,
                    collects_nulls: a.collects_nulls || b.collects_nulls,
                    nullable: a.nullable || b.nullable,
                    operand_typed: a.operand_typed || b.operand_typed,
                })
                .collect(),
        });
    }
    read
}

/// An ungrouped aggregate or an unconditional EXISTS can guarantee a row. An INSERT cannot, as a
/// BEFORE trigger may skip it and SQLite may ignore a conflict by a constraint on the table.
pub fn single_row(tokens: &[String], engine: Engine) -> bool {
    let aggregates = engine.aggregates();
    let tokens = leveled(tokens);
    let branches = branches(&tokens);
    let [branch] = branches.as_slice() else {
        return false;
    };
    match (kind(branch), list(branch)) {
        (Some((at, "select")), Some(range)) => {
            let list = &branch[range.clone()];
            let exists = top(branch, &["from", "where"]).is_none()
                && (list.split(|&(d, t)| d == 0 && t == ",")).all(|item| match item {
                    [(_, exists), (_, "("), ..] => is(exists, &["exists"]),
                    [(_, not), (_, exists), (_, "("), ..] => is(not, &["not"]) && is(exists, &["exists"]),
                    _ => false,
                });
            // What follows a statement in parentheses limits it as its own clauses do:
            // `(SELECT count(*) FROM t) LIMIT 0`
            collapses(&tokens)
                && collapses(branch)
                && (exists || aggregated(list, &aggregates))
                && scalar_projection(branch, at, range.end, &aggregates)
        }
        _ => false,
    }
}

fn scalar_projection(branch: &[Leveled], select: usize, end: usize, aggregates: &Aggregates) -> bool {
    let tail = &branch[end..];
    aggregates.engine != Engine::PostgreSql
        || (scalar_calls(&branch[select..end], aggregates)
            && top(tail, &["order"]).is_none_or(|order| scalar_calls(&tail[order..], aggregates)))
}

/// PostgreSQL evaluates a set-returning call after grouping, even in an expression used only to sort the rows.
/// A scalar subquery hides its own calls, and PostgreSQL forbids a set-returning argument to an aggregate.
fn scalar_calls(tokens: &[Leveled], aggregates: &Aggregates) -> bool {
    let mut at = 0;
    while at < tokens.len() {
        if let Some((inner, _)) = parenthesised(&tokens[at..]) {
            let name = at.checked_sub(1).map_or("", |i| tokens[i].1);
            let schema = at.checked_sub(3).filter(|_| tokens[at - 2].1 == ".");
            let builtin = schema.is_none_or(|i| is(tokens[i].1, &["pg_catalog"]));
            let aggregate = builtin && (is(name, aggregates.nullable) || is(name, aggregates.never_null));
            let syntax = schema.is_none()
                && is(
                    name,
                    &[
                        "select", "as", "in", "exists", "any", "all", "some", "cast", "coalesce", "nullif", "filter",
                        "over", "by", "where", "from",
                    ],
                );
            if !aggregate && is_word(name) && !syntax {
                return false;
            }
            if aggregate || is_statement(inner) {
                at += inner.len() + 1;
            }
        }
        at += 1;
    }
    true
}

/// The tables, by name as written, that every row of the result has a row of. In a `SELECT` those are the tables its
/// `FROM` names outright or inner joins; in an `INSERT`, `UPDATE` or `DELETE` that returns its rows, the table it
/// writes plus those an `UPDATE`'s `FROM` names that way.
///
/// A name proves nothing where it may be another's: a table an outer join may leave without a row, an alias, a CTE.
/// Neither does a statement whose `FROM` we do not read ([`sources`]), a set operation, nor (where a column may stand
/// beside an ungrouped aggregate) one that aggregates no rows into a row of NULLs.
pub fn present(tokens: &[String], aggregates: &Aggregates) -> Vec<String> {
    let branches = branches(&leveled(tokens));
    let [branch] = branches.as_slice() else {
        return Vec::new();
    };
    let Some((ctes, sources)) = sources_of(branch, aggregates) else {
        return Vec::new();
    };
    // A name proves nothing when a CTE or an alias spells it (`WITH a AS (...)`, `LEFT JOIN c AS b`), or when an outer
    // join brings a table of that name too
    let missing = |name: &String| {
        let spells = |other: &Option<String>| (other.as_ref()).is_some_and(|other| other.eq_ignore_ascii_case(name));
        ctes.iter().any(|cte| cte.eq_ignore_ascii_case(name))
            || (sources.iter()).any(|source| spells(&source.alias) || (source.outer && spells(&source.table)))
    };
    (sources.iter())
        .filter(|source| !source.outer)
        .filter_map(|source| source.table.clone())
        .filter(|table| !missing(table))
        .collect()
}

/// Whether `item`, which sqlc reports as a NOT NULL column of `table`, may be NULL all the same: it is that bare
/// column, and every source it may come from is one an outer join may leave without a row. sqlc misses this left of a
/// `RIGHT` or `FULL` join of several tables, for a subquery's rows, and on SQLite and MySQL for what a `LEFT` join
/// brings once a `JOIN` follows.
///
/// Its sources are the one its qualifier names. Without a qualifier they are those of its table or, when no source
/// names that table, those that name none (the subqueries), as sqlc reports a subquery's column as its own table's.
/// Anything but a bare column stays as sqlc reports it: to sqlc a `coalesce` of the column is the column, and rightly
/// NOT NULL.
fn outer(item: &[Leveled], table: &str, sources: &[Source]) -> bool {
    let spells = |name: &Option<String>, by: &str| (name.as_ref()).is_some_and(|name| name.eq_ignore_ascii_case(by));

    let item_sources: Vec<&Source> = match column(item).unwrap_or_default().as_slice() {
        // `schema.table.column` is its table's, as `table.column` is
        [.., source, _] => {
            let source = unquoted(source);
            let exposed = |s: &&Source| match &s.alias {
                Some(_) => spells(&s.alias, &source),
                None => spells(&s.table, &source),
            };
            sources.iter().filter(exposed).collect()
        }
        [_] if !table.is_empty() => {
            // Tables of one name in two schemas are told apart by the schema sqlc reports, where the statement names it
            let (schema, table) = table
                .rsplit_once('.')
                .map_or((None, table), |(schema, table)| (Some(schema), table));
            let of_schema = |source: &&Source| match (&source.schema, schema) {
                (Some(written), Some(reported)) => written.eq_ignore_ascii_case(reported),
                _ => true,
            };
            let own: Vec<&Source> = (sources.iter())
                .filter(|source| spells(&source.table, table) && of_schema(source))
                .collect();
            match own.is_empty() {
                true => (sources.iter()).filter(|source| source.table.is_none()).collect(),
                false => own,
            }
        }
        _ => return false,
    };
    !item_sources.is_empty() && item_sources.iter().all(|source| source.outer)
}

/// The words of the dotted name that `item` is, where it is a bare column with at most an alias.
fn column<'t>(item: &[Leveled<'t>]) -> Option<Vec<&'t str>> {
    let (mut words, mut rest) = (Vec::new(), item);
    while let [(_, word), after @ ..] = rest
        && is_word(word)
    {
        words.push(*word);
        rest = after;
        match after {
            [(_, "."), more @ ..] => rest = more,
            _ => break,
        }
    }
    (!words.is_empty() && is_alias(rest)).then_some(words)
}

/// Plain placeholders compared positionally to columns in a PostgreSQL row comparison. A cast on the
/// placeholder gives sqlc its own type, so we leave those out.
pub(super) fn row_parameters(words: &[String]) -> Vec<(i32, String)> {
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

/// The names of the statement's CTEs, and the sources its result's rows are made of: those of a `SELECT`'s `FROM`, or
/// for an `INSERT`, `UPDATE` or `DELETE` that returns its rows the table it writes, plus those an `UPDATE`'s `FROM` or
/// a `DELETE`'s `USING` names. No sources where it reads none, as a `SELECT` without a `FROM`. `None` where we do not
/// read them ([`sources`]), or where a column may stand beside an ungrouped aggregate and the statement aggregates no
/// rows into a row of NULLs.
fn sources_of(branch: &[Leveled], aggregates: &Aggregates) -> Option<(Vec<String>, Vec<Source>)> {
    const TARGET_ENDS: &[&str] = &[
        "(",
        "set",
        "using",
        "where",
        "values",
        "default",
        "select",
        "with",
        "on",
        "returning",
        ";",
    ];
    const READ_ENDS: &[&str] = &["where", "returning", ";"];

    let (at, kind) = kind(branch)?;
    let statement = &branch[at..];
    let clause = |starts: &[&str], ends: &[&str]| {
        let clause = top(statement, starts).map_or(&[][..], |start| &statement[start + 1..]);
        &clause[..top(clause, ends).unwrap_or(clause.len())]
    };

    // What the statement writes, then what it reads beside it
    let (target, read): (&[Leveled], &[Leveled]) = match kind {
        "select" => {
            let list = list(statement)?;
            let nulls = aggregates.engine != Engine::PostgreSql
                && !grouped(statement)
                && aggregates_rows(statement, &statement[list.clone()], aggregates);
            let after = &statement[list.end..];
            let read = match after {
                [(_, from), read @ ..] if is(from, &["from"]) => read,
                // Without a `FROM` there is nothing to read, but one after an `INTO` is one we do not
                _ if top(after, &["from"]).is_none() => &[],
                _ => return None,
            };
            let (read, after) = read.split_at(top(read, ENDS).unwrap_or(read.len()));
            // A join after what we took as the end of the clause means it was none, as MySQL and SQLite allow a column
            // named as a clause (`offset`)
            if nulls || top(after, &["join"]).is_some() {
                return None;
            }
            (&[], read)
        }
        _ if top(statement, &["returning"]).is_none() => return None,
        "update" => (clause(&["update"], TARGET_ENDS), clause(&["from"], READ_ENDS)),
        "delete" => (clause(&["from"], TARGET_ENDS), clause(&["using"], READ_ENDS)),
        _ => (clause(&["into"], TARGET_ENDS), &[]),
    };

    let ctes = (branch[..at].iter())
        .filter(|&&(d, t)| d == 0 && is_word(t))
        .map(|&(_, t)| unquoted(t))
        .collect();
    let sources = [target, read]
        .into_iter()
        .filter(|clause| !clause.is_empty())
        .map(sources)
        .collect::<Option<Vec<_>>>()?;
    Some((ctes, sources.concat()))
}

/// A token with the depth of parentheses and brackets it stands at.
type Leveled<'t> = (usize, &'t str);

/// A parenthesis or a bracket itself stands at the depth outside it.
fn leveled(tokens: &[String]) -> Vec<Leveled<'_>> {
    let mut depth = 0usize;
    (tokens.iter())
        .map(|token| {
            if matches!(token.as_str(), ")" | "]") {
                depth = depth.saturating_sub(1);
            }
            let level = depth;
            if matches!(token.as_str(), "(" | "[") {
                depth += 1;
            }
            (level, token.as_str())
        })
        .collect()
}

/// Tokens from inside parentheses, leveled again so that their own top is 0.
fn lowered<'t>(tokens: &[Leveled<'t>]) -> Vec<Leveled<'t>> {
    let depth = tokens.first().map_or(0, |&(d, _)| d);
    (tokens.iter()).map(|&(d, t)| (d.saturating_sub(depth), t)).collect()
}

/// Whether `token` is one of `words`, ignoring case.
fn is(token: &str, words: &[&str]) -> bool {
    words.iter().any(|word| token.eq_ignore_ascii_case(word))
}

/// Whether the token at `at` is a name whatever it spells (`k.group`, `AS order`), though not after a number's dot
/// (`1. GROUP BY`).
fn is_name_at(tokens: &[Leveled], at: usize) -> bool {
    match &tokens[..at] {
        [.., (_, number), (_, ".")] => !number.starts_with(|c: char| c.is_ascii_digit()),
        [.., (_, before)] => is(before, &[".", "as"]),
        [] => false,
    }
}

/// Where the first of `words` stands at the top level of `tokens` as a keyword: not as a name, and not as the `FROM` of
/// `IS DISTINCT FROM` or the `GROUP` of `WITHIN GROUP`.
fn top(tokens: &[Leveled], words: &[&str]) -> Option<usize> {
    let mut before = "";
    (0..tokens.len()).find(|&at| {
        let (depth, token) = tokens[at];
        if depth != 0 {
            return false;
        }
        let name = is_name_at(tokens, at);
        let operator = is(before, &["distinct", "within"]) && is(token, &["from", "group"]);
        before = match name {
            true => "",
            false => token,
        };
        is(token, words) && !name && !operator
    })
}

/// What the parentheses `tokens` open with hold, and what follows them.
fn parenthesised<'b, 't>(tokens: &'b [Leveled<'t>]) -> Option<(&'b [Leveled<'t>], &'b [Leveled<'t>])> {
    let [(depth, "("), rest @ ..] = tokens else {
        return None;
    };
    let close = rest.iter().position(|&(d, t)| d == *depth && t == ")")?;
    Some((&rest[..close], &rest[close + 1..]))
}

/// What opens a statement in parentheses, which makes them a subquery.
const QUERY: &[&str] = &["select", "with", "values"];

/// Whether `inner`, what parentheses hold, is a statement, possibly in parentheses of its own with at most what orders
/// or limits it after them.
fn is_statement(mut inner: &[Leveled]) -> bool {
    // A loop and not recursion, for the same reason as in `branches`
    loop {
        match (inner, parenthesised(inner)) {
            ([(_, first), ..], _) if is(first, QUERY) => return true,
            (_, Some((held, after))) if after.first().is_none_or(|&(_, t)| is(t, TAIL)) => {
                inner = held;
            }
            _ => return false,
        }
    }
}

const SET_OPERATIONS: &[&str] = &["union", "intersect", "except"];

/// What may follow a statement in parentheses and still belong to it.
const TAIL: &[&str] = &["order", "limit", "offset", "fetch"];

/// What ends a result list and a `FROM` clause. Not `FOR`, as MySQL's index hints have one too
/// (`USE INDEX FOR JOIN (...)`), and a `FOR UPDATE` that ends the statement names no source anyway.
const ENDS: &[&str] = &[
    "from", "into", "where", "group", "having", "window", "order", "limit", "offset", "fetch", ";",
];

/// The statement itself, or each branch of a `SELECT`'s set operations (one in parentheses as what it holds), in no
/// particular order.
fn branches<'t>(tokens: &[Leveled<'t>]) -> Vec<Vec<Leveled<'t>>> {
    // An `INSERT ... SELECT ... UNION ...` is one statement, whose list is its `RETURNING`
    if kind(tokens).is_some_and(|(_, kind)| kind != "select") {
        return vec![tokens.to_vec()];
    }

    let mut branches = Vec::new();
    // We loop instead of recursing, as a statement may nest its parentheses deeper than our stack is!
    let mut held = vec![tokens.to_vec()];
    while let Some(tokens) = held.pop() {
        let mut rest = &tokens[..];
        while !rest.is_empty() {
            let end = top(rest, SET_OPERATIONS);
            let branch = match &rest[..end.unwrap_or(rest.len())] {
                [(_, quantifier), rest @ ..] if is(quantifier, &["all", "distinct"]) => rest,
                branch => branch,
            };
            // What orders or limits it may follow the parentheses, and then belongs to the statement inside
            let inner = parenthesised(branch)
                .filter(|(_, after)| (after.first()).is_none_or(|&(_, t)| t == ";" || is(t, TAIL)));
            match inner {
                Some((inner, after)) => held.push([&lowered(inner), after].concat()),
                None => branches.push(branch.to_vec()),
            }
            rest = end.map_or(&[], |end| &rest[end + 1..]);
        }
    }
    branches
}

/// The statement's first top-level `SELECT`, `INSERT`, `REPLACE`, `UPDATE` or `DELETE` (after any `WITH`), and where
/// it stands.
fn kind(tokens: &[Leveled]) -> Option<(usize, &'static str)> {
    const KINDS: [&str; 5] = ["select", "insert", "replace", "update", "delete"];
    let at = top(tokens, &KINDS)?;
    let kind = KINDS.into_iter().find(|kind| is(tokens[at].1, &[kind]))?;
    Some((at, kind))
}

/// Where the statement's result list is: after `SELECT` and any `DISTINCT` up to the clause that ends it, or after
/// `RETURNING`.
fn list(tokens: &[Leveled]) -> Option<Range<usize>> {
    let (at, kind) = kind(tokens)?;
    if kind != "select" {
        let start = top(tokens, &["returning"])? + 1;
        let end = top(&tokens[start..], &[";"]).map_or(tokens.len(), |end| start + end);
        return Some(start..end);
    }
    let mut start = at + 1;
    if tokens.get(start).is_some_and(|&(_, t)| is(t, &["distinct", "all"])) {
        start += 1;
        // `DISTINCT ON (...)`
        if tokens.get(start).is_some_and(|&(_, t)| is(t, &["on"])) {
            start += top(&tokens[start..], &[")"])? + 1;
        }
    }
    let end = top(&tokens[start..], ENDS).map_or(tokens.len(), |end| start + end);
    Some(start..end)
}

/// Whether nothing groups the statement's rows, filters the groups or limits the result, so that an aggregating list
/// returns its one row.
fn collapses(tokens: &[Leveled]) -> bool {
    top(tokens, &["group", "having", "window", "limit", "offset", "fetch"]).is_none()
}

/// Whether the statement groups its rows, so that each aggregate has a row at least. The empty grouping set does not
/// count, as it makes a row of no rows: `GROUP BY ()`, and what a `ROLLUP`, a `CUBE` or `GROUPING SETS` may hold.
fn grouped(tokens: &[Leveled]) -> bool {
    top(tokens, &["group"]).is_some_and(|at| {
        let grouping = &tokens[at..];
        let sets = match grouping {
            [_, _, (_, quantifier), sets @ ..] if is(quantifier, &["all", "distinct"]) => sets,
            [_, _, sets @ ..] => sets,
            _ => grouping,
        };
        !matches!(sets, [(_, "("), (_, ")"), ..]) && !rolled(tokens)
    })
}

/// Whether the statement groups its rows by several sets of its keys (`ROLLUP`, `CUBE`, `GROUPING SETS`, MySQL's
/// `WITH ROLLUP`), where a key is NULL in the rows of each set that leaves it out.
fn rolled(tokens: &[Leveled]) -> bool {
    top(tokens, &["group"]).is_some_and(|at| (tokens[at..].iter()).any(|&(_, t)| is(t, &["rollup", "cube", "sets"])))
}

/// What may be NULL in an item though sqlc reports NOT NULL ([`found`]).
struct Found {
    aggregate: bool,
    /// An aggregate other than `count` of no rows, a subquery without a row unless it counts all of them, a `CASE`
    /// that may take no branch or a `NULL` one, or a `nullif` of equal arguments.
    nullable: bool,
    /// Whether it is inside what is never NULL, whatever it holds: a `coalesce` that ends in a default, or a `count`.
    guarded: bool,
}

/// Each aggregate that `tokens` call, each scalar subquery they hold, and each `CASE` or `nullif` that may be NULL by
/// its form, outside every subquery.
///
/// A call with `OVER` is a window function, of each row, and one qualified by any schema but `pg_catalog` is that
/// schema's own function. A subquery that is no value does not count: one read by `EXISTS`, `IN`, `ANY` and the like,
/// the elements of an `ARRAY(...)`, or one compared with `IS`, as a `nullif` may be too.
fn found(tokens: &[Leveled], aggregates: &Aggregates, whole: impl Fn(&str, &[Leveled]) -> bool) -> Vec<Found> {
    let mut found = Vec::new();
    // Whether each parenthesis open here is a `coalesce` with a default, or a `count`
    let mut guards: Vec<bool> = Vec::new();
    let mut at = 0;
    while at < tokens.len() {
        let (_, token) = tokens[at];
        let before = at.checked_sub(1).map_or("", |before| tokens[before].1);
        match (token, parenthesised(&tokens[at..])) {
            ("(", Some((inner, after))) if is_statement(inner) => {
                let after = after.first().map_or("", |&(_, t)| t);
                let value = !is(before, &["exists", "in", "any", "all", "some", "array"]) && !is(after, &["is"]);
                // Limited from outside its parentheses, a count may have no row: `((SELECT count(*) FROM t) LIMIT 0)`
                let counts = !is(after, SET_OPERATIONS)
                    && !is(after, TAIL)
                    && selects(inner, aggregates)
                        .is_some_and(|(call, collapses)| collapses && is(&call, aggregates.never_null));
                if value {
                    found.push(Found {
                        aggregate: false,
                        nullable: !counts,
                        guarded: guards.contains(&true),
                    });
                }
                at += inner.len() + 1;
            }
            ("(", arguments) => {
                let default = arguments.is_some_and(|(arguments, _)| defaults(arguments));
                // What a count counts may be NULL, the count is not
                let qualified = at.checked_sub(2).is_some_and(|dot| tokens[dot].1 == ".");
                let counts = !qualified && is(&unquoted(before), aggregates.never_null);
                guards.push(counts || (default && is(before, &["coalesce", "ifnull"])));
            }
            (")", _) => {
                guards.pop();
            }
            (case, _) if is(case, &["case"]) && !is_name_at(tokens, at) => {
                if case_may_be_null(&tokens[at..]) {
                    found.push(Found {
                        aggregate: false,
                        nullable: true,
                        guarded: guards.contains(&true),
                    });
                }
            }
            (nullif, _) if is(nullif, &["nullif"]) && !is_name_at(tokens, at) => {
                // One compared with `IS` is no value of the item, as a subquery is not
                let tested = parenthesised(&tokens[at + 1..])
                    .is_some_and(|(_, after)| matches!(after, [(_, is_), ..] if is(is_, &["is"])));
                if !tested {
                    found.push(Found {
                        aggregate: false,
                        nullable: true,
                        guarded: guards.contains(&true),
                    });
                }
            }
            (name, _) => {
                let name = unquoted(name);
                let nullable = is(&name, aggregates.nullable);
                // A function of another schema named like an aggregate is that schema's own
                let schema = at.checked_sub(2).filter(|_| before == ".");
                let builtin = schema.is_none_or(|schema| is(tokens[schema].1, &["pg_catalog"]));
                let value_of_window = is(&name, &["lag", "lead", "first_value", "last_value", "nth_value"]);
                if (nullable || is(&name, aggregates.never_null) || value_of_window)
                    && builtin
                    && let Some((arguments, after)) = parenthesised(&tokens[at + 1..])
                    && !(is(&name, aggregates.scalar_of_several) && several(arguments))
                {
                    // An ordered-set aggregate names its order before its filter
                    let after = match after {
                        [(_, within), (_, group), order @ ..] if is(within, &["within"]) && is(group, &["group"]) => {
                            parenthesised(order).map_or(after, |(_, after)| after)
                        }
                        after => after,
                    };
                    // A window preserves each row. Its frame may hold no value, and a lag or lead may have no
                    // row at its offset even when the argument is NOT NULL.
                    let window = over(after).0;
                    if window || !value_of_window {
                        found.push(Found {
                            aggregate: !window,
                            nullable: (nullable || value_of_window)
                                && (window
                                    || matches!(after, [(_, filter), ..] if is(filter, &["filter"]))
                                    || !whole(&name, arguments)),
                            guarded: guards.contains(&true),
                        });
                    }
                }
            }
        }
        at += 1;
    }
    found
}

/// Whether the `CASE` that `tokens` open with may be NULL by its form: without an `ELSE` it is where no `WHEN` holds,
/// and a branch may be the literal `NULL`. We do not read one without an `END`, so that one may be NULL too.
fn case_may_be_null(tokens: &[Leveled]) -> bool {
    let depth = tokens.first().map_or(0, |&(d, _)| d);
    let (mut nested, mut otherwise) = (0usize, false);
    // Where the value of the branch being read starts
    let mut value: Option<usize> = None;
    for at in 0..tokens.len() {
        let (level, token) = tokens[at];
        if level < depth {
            break;
        }
        if level > depth || is_name_at(tokens, at) {
            continue;
        }
        if is(token, &["case"]) {
            nested += 1;
        } else if nested > 1 {
            nested -= usize::from(is(token, &["end"]));
        } else if is(token, &["when", "then", "else", "end"]) {
            if value.take().is_some_and(|start| is_null(&tokens[start..at])) {
                return true;
            }
            if is(token, &["end"]) {
                return !otherwise;
            }
            if is(token, &["then", "else"]) {
                value = Some(at + 1);
                otherwise |= is(token, &["else"]);
            }
        }
    }
    true
}

/// A nonempty group can still contain only NULLs. We can prove a value for these aggregates of a required
/// column or literal. Sample statistics, for example, need more than one observation.
fn group_value(name: &str, arguments: &[Leveled], required: impl Fn(&[Leveled]) -> Option<bool>) -> bool {
    is(
        name,
        &[
            "min", "max", "sum", "avg", "bool_and", "bool_or", "every", "bit_and", "bit_or", "bit_xor",
        ],
    ) && non_null_value(aggregate_input(arguments), required)
}

/// DISTINCT, ALL and the aggregate's ORDER BY leave the input values themselves unchanged.
fn aggregate_input<'b, 't>(arguments: &'b [Leveled<'t>]) -> &'b [Leveled<'t>] {
    let arguments = match arguments {
        [(_, modifier), rest @ ..] if is(modifier, &["distinct", "all"]) => rest,
        other => other,
    };
    let depth = arguments.first().map_or(0, |&(d, _)| d);
    let order = arguments
        .windows(2)
        .position(|pair| pair[0].0 == depth && is(pair[0].1, &["order"]) && is(pair[1].1, &["by"]));
    &arguments[..order.unwrap_or(arguments.len())]
}

fn non_null_value(arguments: &[Leveled], required: impl Fn(&[Leveled]) -> Option<bool>) -> bool {
    required(arguments) == Some(true)
        || is_defaulted(arguments)
        || (!several(arguments) && defaults(arguments))
        || case_literals(arguments).is_some_and(|values| {
            values
                .iter()
                .all(|v| v.starts_with('\'') || is(v, &["true", "false"]) || v.parse::<i32>().is_ok())
        })
}

/// A LEFT JOIN supplies a NULL element even for a group that has a row. An aggregate FILTER can omit that row.
fn collects_nulls(tokens: &[Leveled], required: impl Fn(&[Leveled]) -> Option<bool>) -> bool {
    let tokens = uncast_item(tokens);
    let tokens = match tokens {
        [(_, schema), (_, "."), rest @ ..] if is(schema, &["pg_catalog"]) => rest,
        _ => tokens,
    };
    let [(_, name), rest @ ..] = tokens else { return false };
    if !is(name, &["array_agg"]) {
        return false;
    }
    let Some((arguments, after)) = parenthesised(rest) else {
        return false;
    };
    if !is_alias(uncast(over(after).1)) {
        return false;
    }
    let value = aggregate_input(arguments);
    if non_null_value(value, required) {
        return false;
    }
    let filtered = match after {
        [(_, filter), rest @ ..] if is(filter, &["filter"]) => parenthesised(rest).map(|(predicate, _)| predicate),
        _ => None,
    };
    !matches!(filtered, Some([(_, w), field @ .., (_, i), (_, n), (_, nil)])
        if is(w, &["where"]) && is(i, &["is"]) && is(n, &["not"]) && is(nil, &["null"])
            && column(value).is_some_and(|words| Some(words) == column(field)))
}

/// A bare column's catalog nullability applies only to the table that supplies it. A CTE can shadow that table,
/// and an outer join can supply NULL for every column of the missing row.
fn column_required(
    tokens: &[Leveled],
    ctes: &[String],
    sources: &[Source],
    required: &impl Fn(&str, &str, &str) -> Option<bool>,
) -> Option<bool> {
    let words = column(tokens)?;
    let (source, field) = match words.as_slice() {
        [field] if sources.len() == 1 => (&sources[0], field),
        [qualifier, field] => {
            let qualifier = unquoted(qualifier);
            let mut named = sources
                .iter()
                .filter(|s| s.alias.as_ref().or(s.table.as_ref()) == Some(&qualifier));
            let source = named.next()?;
            if named.next().is_some() {
                return None;
            }
            (source, field)
        }
        _ => return None,
    };
    if source.outer {
        return Some(false);
    }
    let table = source.table.as_ref()?;
    if source.schema.is_none() && ctes.iter().any(|cte| cte.eq_ignore_ascii_case(table)) {
        return None;
    }
    required(source.schema.as_deref().unwrap_or(""), table, &unquoted(field))
}

/// Whether `tokens` are the literal `NULL`, cast or not, with at most an alias. It is NULL whatever sqlc reports of
/// the cast.
fn is_null(tokens: &[Leveled]) -> bool {
    match tokens {
        [(_, null), rest @ ..] if is(null, &["null"]) => is_alias(uncast(rest)),
        [(_, cast), rest @ ..] if is(cast, &["cast"]) => parenthesised(rest).is_some_and(|(inner, after)| {
            matches!(inner, [(_, null), (_, to), ..] if is(null, &["null"]) && is(to, &["as"])) && is_alias(after)
        }),
        _ => false,
    }
}

/// `tokens` without the cast they open with, if it is a cast to one word and its modifiers (`::numeric(12, 2)`). We do
/// not take more words, as those could be an operator's (`0::int OR x`).
fn uncast<'b, 't>(tokens: &'b [Leveled<'t>]) -> &'b [Leveled<'t>] {
    match tokens {
        [(_, ":"), (_, ":"), (_, r#type), modifiers @ ..] if is_word(r#type) => {
            let modifier = |&(_, t): &Leveled| t.starts_with(|c: char| c.is_ascii_digit() || "()[],".contains(c));
            &modifiers[modifiers.iter().take_while(|token| modifier(token)).count()..]
        }
        tokens => tokens,
    }
}

fn aggregated(tokens: &[Leveled], aggregates: &Aggregates) -> bool {
    found(tokens, aggregates, |_, _| false)
        .iter()
        .any(|found| found.aggregate)
}

/// Whether `statement` aggregates its rows: its result `list` calls an aggregate, or a `HAVING` filters the one group
/// they then are.
fn aggregates_rows(statement: &[Leveled], list: &[Leveled], aggregates: &Aggregates) -> bool {
    aggregated(list, aggregates) || top(statement, &["having"]).is_some()
}

/// Whether what follows a call's arguments makes it a window function (`OVER (...)` or `OVER window`, after any
/// `FILTER (...)`), and what follows that. Either word alone is an alias, as SQLite reads it.
fn over<'b, 't>(after: &'b [Leveled<'t>]) -> (bool, &'b [Leveled<'t>]) {
    let after = match after {
        [(_, filter), rest @ ..] if is(filter, &["filter"]) => parenthesised(rest).map_or(after, |(_, after)| after),
        after => after,
    };
    match after {
        [(_, over), rest @ ..] if is(over, &["over"]) => match rest {
            [(_, "("), ..] => (true, parenthesised(rest).map_or(&[], |(_, after)| after)),
            [(_, window), after @ ..] if is_word(window) && !is(window, ENDS) => (true, after),
            _ => (false, after),
        },
        after => (false, after),
    }
}

fn several(arguments: &[Leveled]) -> bool {
    let depth = arguments.first().map_or(0, |&(d, _)| d);
    arguments.iter().any(|&(d, t)| d == depth && t == ",")
}

/// Whether the last of a `coalesce`'s `arguments` is a default: a literal, cast or not, which is never NULL. Anything
/// else may be, as `coalesce(max(x), max(y))` is NULL of no rows.
fn defaults(arguments: &[Leveled]) -> bool {
    let depth = arguments.first().map_or(0, |&(d, _)| d);
    let last = (arguments.rsplit(|&(d, t)| d == depth && t == ","))
        .next()
        .unwrap_or_default();

    // Digits alone, as MySQL's `2nd` is a column's name
    let digits = |token: &str| token.bytes().all(|b| b.is_ascii_digit());
    let unsigned = match last {
        [(_, "-" | "+"), number @ ..] if !number.is_empty() => number,
        last => last,
    };
    let cast = match unsigned {
        [(_, whole), (_, "."), (_, fraction), cast @ ..] if digits(whole) && digits(fraction) => cast,
        [(_, number), cast @ ..] if digits(number) => cast,
        [(_, literal), cast @ ..]
            if unsigned.len() == last.len() && (literal.starts_with('\'') || is(literal, &["true", "false"])) =>
        {
            cast
        }
        _ => return false,
    };
    uncast(cast).is_empty()
}

/// With `bare`, a column can be NULL even outside
/// an aggregate, as happens beside an ungrouped aggregate of no rows or for a key its grouping set leaves out.
fn item(
    tokens: &[Leveled],
    grouped: bool,
    bare: bool,
    aggregates: &Aggregates,
    required: impl Fn(&[Leveled]) -> Option<bool>,
) -> Item {
    let found = found(tokens, aggregates, |name, args| {
        grouped && group_value(name, args, &required)
    });
    let count = match parenthesised(tokens) {
        Some((inner, after)) if is_alias(after) => selects(inner, aggregates).is_some_and(|(call, _)| call == "count"),
        _ => call(tokens).is_some_and(|(call, _)| call == "count"),
    };
    let null = |found: &Found| found.nullable && !found.guarded;
    let column = bare && !found.iter().any(|found| found.aggregate) && !is_defaulted(tokens);

    let mut alone = tokens;
    while let Some((inner, after)) = parenthesised(alone)
        && is_alias(after)
    {
        alone = inner;
    }
    let operand_typed = match alone {
        [(_, case), ..] if is(case, &["case"]) => after_case(alone).is_some_and(is_alias) && !is_literal_case(alone),
        alone => call(alone).is_some_and(|(call, _)| ["coalesce", "nullif"].contains(&call.as_str())),
    };
    let integer = matches!(alone, [(_, number), after @ ..] if number.parse::<i32>().is_ok() && is_alias(after));
    Item {
        count,
        integer,
        collects_nulls: aggregates.engine == Engine::PostgreSql && collects_nulls(tokens, required),
        nullable: column || is_null(tokens) || json_lookup(uncast_item(tokens)) || found.iter().any(null),
        operand_typed,
    }
}

/// Whether `tokens` are a `coalesce` that ends in a default ([`defaults`]), with at most an alias, cast and in
/// parentheses as it may be.
fn is_defaulted(tokens: &[Leveled]) -> bool {
    matches!(uncast_item(tokens), [(_, coalesce), rest @ ..] if is(coalesce, &["coalesce", "ifnull"])
        && parenthesised(rest).is_some_and(|(arguments, after)| defaults(arguments) && is_alias(uncast(after))))
}

/// A cast preserves NULL, as do parentheses around the item.
fn uncast_item<'b, 't>(mut tokens: &'b [Leveled<'t>]) -> &'b [Leveled<'t>] {
    loop {
        tokens = match (tokens, parenthesised(tokens)) {
            (_, Some((inner, after))) if is_alias(uncast(after)) => inner,
            ([(depth, cast), rest @ ..], _) if is(cast, &["cast"]) => match parenthesised(rest) {
                Some((inner, after)) if is_alias(after) => {
                    let to = inner.iter().rposition(|&(d, t)| d == depth + 1 && is(t, &["as"]));
                    &inner[..to.unwrap_or(inner.len())]
                }
                _ => return tokens,
            },
            _ => return tokens,
        };
    }
}

/// A cast preserves a NULL column on each engine even though sqlc marks the cast NOT NULL.
fn cast_input<'b, 't>(tokens: &'b [Leveled<'t>]) -> Option<&'b [Leveled<'t>]> {
    let inner = uncast_item(tokens);
    if let Some(at) = inner.iter().position(|&(_, token)| token == ":")
        && is_alias(uncast(&inner[at..]))
    {
        return Some(&inner[..at]);
    }
    (inner.len() < tokens.len()).then_some(inner)
}

/// PostgreSQL returns NULL for a missing JSON key or array index, even on a NOT NULL column.
fn json_lookup(tokens: &[Leveled]) -> bool {
    let operator = |token: &str| matches!(token, "->" | "->>" | "#>" | "#>>");
    let Some(at) = tokens.iter().position(|&(_, token)| operator(token)) else {
        return false;
    };
    if column(&tokens[..at]).is_none() {
        return false;
    }
    let mut rest = &tokens[at..];
    while let [(_, op), (_, key), after @ ..] = rest
        && operator(op)
        && (key.starts_with('\'') || key.parse::<i32>().is_ok() || key.starts_with('$'))
    {
        rest = after;
    }
    is_alias(rest)
}

/// What follows the `END` of the `CASE` that `tokens` open with.
fn after_case<'b, 't>(tokens: &'b [Leveled<'t>]) -> Option<&'b [Leveled<'t>]> {
    let depth = tokens.first().map_or(0, |&(d, _)| d);
    let mut open = 0usize;
    (0..tokens.len())
        .filter(|&at| tokens[at].0 == depth && !is_name_at(tokens, at))
        .find(|&at| {
            open += usize::from(is(tokens[at].1, &["case"]));
            open -= usize::from(is(tokens[at].1, &["end"]) && open > 0);
            open == 0
        })
        .map(|end| &tokens[end + 1..])
}

/// Whether the `CASE` that `tokens` open with gives a literal of one kind in each branch, an `ELSE` among them: a string
/// in each, or `TRUE` or `FALSE` in each. sqlc's type of its `ELSE` is then the `CASE`'s, as PostgreSQL resolves
/// literals of no type to `text`, and `TRUE` and `FALSE` are `bool` (on SQLite and MySQL sqlc's type of those is a
/// guess we refuse anyway).
fn is_literal_case(tokens: &[Leveled]) -> bool {
    case_literals(tokens).is_some_and(|values| {
        values.iter().all(|v| v.starts_with('\'')) || values.iter().all(|v| is(v, &["true", "false"]))
    })
}

fn case_literals<'t>(tokens: &[Leveled<'t>]) -> Option<Vec<&'t str>> {
    if !tokens.first().is_some_and(|&(_, word)| is(word, &["case"])) {
        return None;
    }
    let depth = tokens.first().map_or(0, |&(d, _)| d);
    let keywords: Vec<usize> = (1..tokens.len())
        .filter(|&at| tokens[at].0 == depth && !is_name_at(tokens, at))
        .filter(|&at| is(tokens[at].1, &["case", "when", "then", "else", "end"]))
        .collect();
    let end = keywords.iter().position(|&at| is(tokens[at].1, &["end"]))?;
    if !is_alias(&tokens[keywords[end] + 1..]) {
        return None;
    }
    let keywords = &keywords[..=end];

    // A `CASE` within, in a condition or a branch, is no literal of ours to read
    if keywords.iter().any(|&at| is(tokens[at].1, &["case"])) {
        return None;
    }
    let results: Vec<&str> = (keywords.windows(2))
        .filter(|pair| is(tokens[pair[0]].1, &["then", "else"]))
        .map(|pair| match &tokens[pair[0] + 1..pair[1]] {
            [(_, literal)] => *literal,
            _ => "",
        })
        .collect();
    let otherwise = keywords.iter().any(|&at| is(tokens[at].1, &["else"]));
    otherwise.then_some(results)
}

/// The function, lowercase, that `tokens` are a bare call of (a name and its arguments, with at most a `FILTER`, an
/// `OVER` and an alias after them), and whether it is a window function ([`over`]).
fn call(tokens: &[Leveled]) -> Option<(String, bool)> {
    let [(_, name), rest @ ..] = tokens else {
        return None;
    };
    let (windowed, after) = over(parenthesised(rest)?.1);
    let bare = is_word(name) && !name.starts_with(['"', '`', '[']);
    (bare && is_alias(after)).then(|| (name.to_lowercase(), windowed))
}

/// The function the subquery `inner` selects a bare call of as its only item, and whether it aggregates every row:
/// nothing groups, filters or limits them, and no window keeps them apart. Of a `count` the subquery then always has
/// its one row.
fn selects(inner: &[Leveled], aggregates: &Aggregates) -> Option<(String, bool)> {
    let inner = lowered(inner);
    let [branch] = <[_; 1]>::try_from(branches(&inner)).ok()?;
    let range = list(&branch)?;
    let (call, windowed) = call(&branch[range.clone()])?;
    let single = !windowed
        && collapses(&inner)
        && collapses(&branch)
        && scalar_projection(&branch, kind(&branch)?.0, range.end, aggregates);
    Some((call, single))
}

/// One source of a `FROM` clause: a table, or the rows of a subquery or a function.
#[derive(Debug, Clone)]
struct Source {
    /// Unquoted and without its schema; `None` for the rows of a subquery or a function.
    table: Option<String>,
    /// The schema the table is qualified by, if it is.
    schema: Option<String>,
    /// `None` too for an alias that is the table's own name.
    alias: Option<String>,
    /// Whether an outer join may leave it without a row.
    outer: bool,
}

/// The sources a `FROM` clause names, each `outer` if a join may leave it without a row: what a `LEFT` join brings,
/// what stands before a `RIGHT` join since the last comma, and both sides of a `FULL` one.
///
/// `None` where we do not read the clause: with a join in parentheses, or a join inside another's, which starts before
/// the other's `ON` (`a LEFT JOIN b JOIN c ON ... ON ...`).
fn sources(from: &[Leveled]) -> Option<Vec<Source>> {
    const SIDES: &[&str] = &["natural", "inner", "cross", "left", "right", "full", "outer"];
    let mut sources: Vec<Source> = Vec::new();
    // Where the sources joined to each other start: after the last comma
    let mut joined = 0;
    // Where the next source starts, and whether an outer join brings it
    let mut next = Some((0, false));
    let mut awaits_on = false;
    for at in 0..from.len() {
        let (depth, token) = from[at];
        if let Some((_, outer)) = next.take_if(|(start, _)| *start == at) {
            let prefix = (from[at..].iter())
                .take_while(|&&(_, t)| is(t, &["only", "lateral"]))
                .count();
            let (table, schema, rest) = match &from[at + prefix..] {
                source @ [(_, "("), ..] => {
                    let (inner, rest) = parenthesised(source)?;
                    // A subquery, as anything else is a join in parentheses
                    is_statement(inner).then_some((None, None, rest))?
                }
                source => {
                    let (name, schema, rest) = name(source)?;
                    match parenthesised(rest) {
                        // A function called for its rows is no table
                        Some((_, rest)) => (None, None, rest),
                        None => (Some(name), schema, rest),
                    }
                }
            };
            // A table alias can rename its columns by position. Their new names tell us nothing about the
            // catalog column with the same name.
            let alias = leading_alias(rest);
            let renamed = alias.is_some()
                && (matches!(rest, [(_, a), _, (_, "("), ..] if is(a, &["as"])) || matches!(rest, [_, (_, "("), ..]));
            let table = table.filter(|_| !renamed);
            let own = |alias: &String| (table.as_ref()).is_some_and(|table| table.eq_ignore_ascii_case(alias));
            sources.push(Source {
                alias: alias.filter(|alias| !own(alias)),
                table,
                schema,
                outer,
            });
            continue;
        }
        if depth != 0 || is_name_at(from, at) {
            continue;
        }
        if token == "," {
            (joined, next) = (sources.len(), Some((at + 1, false)));
        } else if is(token, &["on", "using"]) {
            awaits_on = false;
        } else if is(token, &["join", "straight_join"]) {
            let sides: Vec<&str> = (from[..at].iter().rev())
                .take_while(|&&(d, t)| d == 0 && is(t, SIDES))
                .map(|&(_, t)| t)
                .collect();
            let side = |side: &str| sides.iter().any(|s| is(s, &[side]));
            if awaits_on {
                return None;
            }
            if side("right") || side("full") {
                (sources[joined..].iter_mut()).for_each(|source| source.outer = true);
            }
            awaits_on = !side("cross") && !side("natural");
            next = Some((at + 1, side("left") || side("full")));
        }
    }
    Some(sources)
}

/// The name `tokens` open with, unquoted, with the schema it is qualified by (`schema.table`), and what follows it.
fn name<'b, 't>(tokens: &'b [Leveled<'t>]) -> Option<(String, Option<String>, &'b [Leveled<'t>])> {
    let (mut rest, mut schema) = (tokens, None);
    loop {
        let [(_, name), after @ ..] = rest else {
            return None;
        };
        match after {
            _ if !is_word(name) => return None,
            [(_, "."), after @ ..] => (rest, schema) = (after, Some(unquoted(name))),
            after => return Some((unquoted(name), schema, after)),
        }
    }
}

/// The alias `tokens` open with, if they do: `AS alias`, or a bare word that is no keyword of a `FROM`.
fn leading_alias(tokens: &[Leveled]) -> Option<String> {
    const KEYWORDS: &[&str] = &[
        "on",
        "using",
        "join",
        "straight_join",
        "natural",
        "inner",
        "cross",
        "left",
        "right",
        "full",
        "outer",
    ];
    let alias = match tokens {
        [(_, r#as), (_, alias), ..] if is(r#as, &["as"]) => alias,
        [(_, alias), ..] => alias,
        [] => return None,
    };
    (is_word(alias) && !is(alias, KEYWORDS)).then(|| unquoted(alias))
}

fn unquoted(name: &str) -> String {
    name.trim_matches(['"', '`', '[', ']']).to_string()
}

/// Whether `tokens` are at most an alias: nothing, `alias`, or `AS alias`.
fn is_alias(tokens: &[Leveled]) -> bool {
    match tokens {
        [] => true,
        [(_, alias)] => is_word(alias),
        [(_, r#as), (_, alias)] => is(r#as, &["as"]) && is_word(alias),
        _ => false,
    }
}

/// Whether `token` is a name or an alias, quoted or not.
fn is_word(token: &str) -> bool {
    token.starts_with(|c: char| c.is_alphabetic() || "_\"`[".contains(c))
}
