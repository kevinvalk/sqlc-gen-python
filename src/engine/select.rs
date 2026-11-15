//! We read a statement's tokens where sqlc's report of its result is not enough. sqlc types a call by the function's
//! first overload without saying which function it called. It reports a scalar subquery and every cast NOT NULL,
//! though a subquery without a row, an aggregate of no rows and a `CASE` where no branch holds are NULL. It reports
//! every `sqlc.embed` nullable, joined or not. And it reports NOT NULL the columns of a table an outer join may leave
//! without a row: left of a `RIGHT` or `FULL` join of several tables, and on SQLite and MySQL what a `LEFT` join
//! brings once a `JOIN` follows it.
//!
//! IMPORTANT: Each reader answers only what it can prove, and what it cannot read gives the answer that widens a type
//! (a column that may be NULL, a statement that may return no row, a table that may be missing). A wrong `X | None` is
//! a nuisance, a wrong `X` is a value that is not what its type says.
//!
//! We read a statement at its top level, the tokens outside every parenthesis and bracket. A set operation (`UNION`,
//! `INTERSECT`, `EXCEPT`) splits a `SELECT` into branches we read on their own, and a branch in parentheses as what it
//! holds. A word is only a keyword where it can be one: after a `.` or an `AS` it is a name, whatever it spells.

use std::ops::Range;

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
    /// Whether a column may stand beside an ungrouped aggregate, where it is NULL of no rows: on SQLite, and on MySQL
    /// without `ONLY_FULL_GROUP_BY`.
    pub bare_columns: bool,
}

/// One item of a result list: a column of the result, or one of an embed's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Item {
    /// Whether it is a bare `count(...)` (filtered or over a window as it may be), or a scalar subquery of one alone.
    pub count: bool,
    /// Whether it may be NULL where sqlc reports NOT NULL: a scalar subquery that may find no row, an aggregate of what
    /// may be no rows, a `CASE` that may take no branch or a `NULL` one, the literal `NULL` itself, or a column of a
    /// source an outer join may leave without a row.
    pub nullable: bool,
}

/// What each item of the statement's result list is, by position, where `tables` holds the table sqlc reports for
/// each item's column (empty for none). The list is the top-level `SELECT`'s (after any `WITH`) or the `RETURNING`
/// one. In a set operation an item is a count only if it is one in every branch, and nullable if it is in any. `None`
/// when the list does not read as one item per column, as we then know nothing of any of them.
pub fn items(tokens: &[String], tables: &[&str], aggregates: &Aggregates) -> Option<Vec<Item>> {
    let mut read: Option<Vec<Item>> = None;
    for branch in branches(&leveled(tokens)) {
        let list = &branch[list(&branch)?];
        let split: Vec<&[Leveled]> = list.split(|&(d, t)| d == 0 && t == ",").collect();
        if list.is_empty() || split.len() != tables.len() {
            return None;
        }
        let grouped = grouped(&branch);
        let bare = aggregates.bare_columns && aggregates_rows(&branch, list, aggregates);
        let sources = sources_of(&branch, aggregates).map_or(Vec::new(), |(_, sources)| sources);
        let items = (split.iter().zip(tables)).map(|(tokens, table)| {
            let item = item(tokens, grouped, bare, aggregates);
            Item {
                nullable: item.nullable || outer(tokens, table, &sources),
                ..item
            }
        });
        read = Some(match read {
            None => items.collect(),
            Some(first) => (first.into_iter().zip(items))
                .map(|(a, b)| Item {
                    count: a.count && b.count,
                    nullable: a.nullable || b.nullable,
                })
                .collect(),
        });
    }
    read
}

/// Whether the statement returns exactly one row, whatever the data. That is an `INSERT` of one row of `VALUES` (or
/// `DEFAULT VALUES`) `RETURNING` it, which no `OR IGNORE`, `ON CONFLICT DO NOTHING` or `DO UPDATE ... WHERE` can skip;
/// a `SELECT` whose list calls an aggregate while nothing groups, filters or limits the result; or a `SELECT` of
/// `EXISTS (...)` alone, of no table and unfiltered.
pub fn single_row(tokens: &[String], aggregates: &Aggregates) -> bool {
    let tokens = leveled(tokens);
    let branches = branches(&tokens);
    let [branch] = branches.as_slice() else {
        return false;
    };
    match (kind(branch), list(branch)) {
        (Some((at, "insert" | "replace")), _) => inserts_one(&branch[at..]),
        (Some((_, "select")), Some(list)) => {
            let list = &branch[list];
            let exists = top(branch, &["from", "where"]).is_none()
                && (list.split(|&(d, t)| d == 0 && t == ",")).all(|item| match item {
                    [(_, exists), (_, "("), ..] => is(exists, &["exists"]),
                    [(_, not), (_, exists), (_, "("), ..] => is(not, &["not"]) && is(exists, &["exists"]),
                    _ => false,
                });
            // What follows a statement in parentheses limits it as its own clauses do:
            // `(SELECT count(*) FROM t) LIMIT 0`
            collapses(&tokens) && collapses(branch) && (exists || aggregated(list, aggregates))
        }
        _ => false,
    }
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
        let named = |other: &Option<String>| (other.as_ref()).is_some_and(|other| other.eq_ignore_ascii_case(name));
        ctes.iter().any(|cte| cte.eq_ignore_ascii_case(name))
            || (sources.iter()).any(|source| named(&source.alias) || (source.outer && named(&source.table)))
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
    let named = |name: &Option<String>, by: &str| (name.as_ref()).is_some_and(|name| name.eq_ignore_ascii_case(by));

    // The dotted name the item opens with, and what follows it
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

    let item_sources: Vec<&Source> = match words.as_slice() {
        _ if !alias(rest) => return false,
        // `schema.table.column` is its table's, as `table.column` is
        [.., source, _] => {
            let source = unquoted(source);
            let exposed = |s: &&Source| match &s.alias {
                Some(_) => named(&s.alias, &source),
                None => named(&s.table, &source),
            };
            sources.iter().filter(exposed).collect()
        }
        [_] if !table.is_empty() => {
            let own: Vec<&Source> = (sources.iter()).filter(|source| named(&source.table, table)).collect();
            match own.is_empty() {
                true => (sources.iter()).filter(|source| source.table.is_none()).collect(),
                false => own,
            }
        }
        _ => return false,
    };
    !item_sources.is_empty() && item_sources.iter().all(|source| source.outer)
}

/// The names of the statement's CTEs, and the sources its result's rows are made of: those of a `SELECT`'s `FROM`, or
/// for an `INSERT`, `UPDATE` or `DELETE` that returns its rows the table it writes, plus those an `UPDATE`'s `FROM` or
/// a `DELETE`'s `USING` names. `None` where we do not read them ([`sources`]), or where a column may stand beside an
/// ungrouped aggregate and the statement aggregates no rows into a row of NULLs.
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
            let nulls = aggregates.bare_columns
                && !grouped(statement)
                && aggregates_rows(statement, &statement[list.clone()], aggregates);
            let [(_, from), rest @ ..] = &statement[list.end..] else {
                return None;
            };
            let (read, after) = rest.split_at(top(rest, ENDS).unwrap_or(rest.len()));
            // A join after what we took as the end of the clause means it was none, as MySQL and SQLite allow a column
            // named as a clause (`offset`)
            if !is(from, &["from"]) || nulls || top(after, &["join"]).is_some() {
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
fn named(tokens: &[Leveled], at: usize) -> bool {
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
        let name = named(tokens, at);
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
        !matches!(sets, [(_, "("), (_, ")"), ..])
            && !(grouping.iter()).any(|&(_, t)| is(t, &["rollup", "cube", "sets"]))
    })
}

/// What may be NULL in an item though sqlc reports NOT NULL ([`found`]).
struct Found {
    aggregate: bool,
    /// An aggregate other than `count` of no rows, a subquery without a row unless it counts all of them, or a `CASE`
    /// that may take no branch or a `NULL` one.
    nullable: bool,
    /// Whether it is inside what is never NULL, whatever it holds: a `coalesce` that ends in a default, or a `count`.
    guarded: bool,
}

/// Each aggregate that `tokens` call, each scalar subquery they hold, and each `CASE` that may be NULL by its form,
/// outside every subquery.
///
/// A call with `OVER` is a window function, of each row, and one qualified by any schema but `pg_catalog` is that
/// schema's own function. A subquery that is no value does not count: one read by `EXISTS`, `IN`, `ANY` and the like,
/// the elements of an `ARRAY(...)`, or one compared with `IS`.
fn found(tokens: &[Leveled], aggregates: &Aggregates) -> Vec<Found> {
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
                    && selects(inner).is_some_and(|(call, collapses)| collapses && is(&call, aggregates.never_null));
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
            (case, _) if is(case, &["case"]) && !named(tokens, at) => {
                if case_may_be_null(&tokens[at..]) {
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
                if (nullable || is(&name, aggregates.never_null))
                    && builtin
                    && let Some((arguments, after)) = parenthesised(&tokens[at + 1..])
                    && !over(after).0
                    && !(is(&name, aggregates.scalar_of_several) && several(arguments))
                {
                    found.push(Found {
                        aggregate: true,
                        nullable,
                        guarded: guards.contains(&true),
                    });
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
        if level > depth || named(tokens, at) {
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

/// Whether `tokens` are the literal `NULL`, cast or not, with at most an alias. It is NULL whatever sqlc reports of
/// the cast.
fn is_null(tokens: &[Leveled]) -> bool {
    match tokens {
        [(_, null), rest @ ..] if is(null, &["null"]) => alias(uncast(rest)),
        [(_, cast), rest @ ..] if is(cast, &["cast"]) => parenthesised(rest).is_some_and(|(inner, after)| {
            matches!(inner, [(_, null), (_, to), ..] if is(null, &["null"]) && is(to, &["as"])) && alias(after)
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
    found(tokens, aggregates).iter().any(|found| found.aggregate)
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

/// What `tokens`, one item of a result list, are. `grouped` when each group of the statement has a row, and `bare` when
/// a column beside an ungrouped aggregate is NULL of no rows.
fn item(tokens: &[Leveled], grouped: bool, bare: bool, aggregates: &Aggregates) -> Item {
    let found = found(tokens, aggregates);
    let count = match parenthesised(tokens) {
        Some((inner, after)) if alias(after) => selects(inner).is_some_and(|(call, _)| call == "count"),
        _ => call(tokens).is_some_and(|(call, _)| call == "count"),
    };
    let null = |found: &Found| found.nullable && !found.guarded && !(found.aggregate && grouped);
    let column = bare && !grouped && !found.iter().any(|found| found.aggregate);
    Item {
        count,
        nullable: column || is_null(tokens) || found.iter().any(null),
    }
}

/// The function, lowercase, that `tokens` are a bare call of (a name and its arguments, with at most a `FILTER`, an
/// `OVER` and an alias after them), and whether it is a window function ([`over`]).
fn call(tokens: &[Leveled]) -> Option<(String, bool)> {
    let [(_, name), rest @ ..] = tokens else {
        return None;
    };
    let (windowed, after) = over(parenthesised(rest)?.1);
    let bare = is_word(name) && !name.starts_with(['"', '`', '[']);
    (bare && alias(after)).then(|| (name.to_lowercase(), windowed))
}

/// The function the subquery `inner` selects a bare call of as its only item, and whether it aggregates every row:
/// nothing groups, filters or limits them, and no window keeps them apart. Of a `count` the subquery then always has
/// its one row.
fn selects(inner: &[Leveled]) -> Option<(String, bool)> {
    let inner = lowered(inner);
    let [branch] = <[_; 1]>::try_from(branches(&inner)).ok()?;
    let (call, windowed) = call(&branch[list(&branch)?])?;
    Some((call, !windowed && collapses(&inner) && collapses(&branch)))
}

/// Whether `tokens`, an `INSERT` from its `INSERT` on, return the one row it inserts.
fn inserts_one(tokens: &[Leveled]) -> bool {
    let Some(returning) = top(tokens, &["returning"]) else {
        return false;
    };

    // We cut at `RETURNING` first, as a returned column may be named `values` or `do`
    let head = &tokens[..returning];
    let Some(values) = top(head, &["values"]) else {
        return false;
    };
    let row = &head[values + 1..];
    let row = &row[..top(row, &["on"]).unwrap_or(row.len())];
    let rows = row.iter().filter(|&&(d, t)| d == 0 && t == "(").count();
    let defaults = values > 0 && is(head[values - 1].1, &["default"]);

    // What may leave the row out: SQLite's `INSERT OR IGNORE`, `DO NOTHING`, and the `WHERE` of a `DO UPDATE`
    let ignores = matches!(head, [_, (_, or), (_, ignore), ..] if is(or, &["or"]) && is(ignore, &["ignore"]));
    let skips = top(head, &["do"]).is_some_and(|action| top(&head[action..], &["nothing", "where"]).is_some());
    (rows == 1 || (defaults && rows == 0)) && !ignores && !skips
}

/// One source of a `FROM` clause: a table, or the rows of a subquery or a function.
#[derive(Debug, Clone)]
struct Source {
    /// Unquoted and without its schema; `None` for the rows of a subquery or a function.
    table: Option<String>,
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
            let (table, rest) = match &from[at + prefix..] {
                source @ [(_, "("), ..] => {
                    let (inner, rest) = parenthesised(source)?;
                    // A subquery, as anything else is a join in parentheses
                    is_statement(inner).then_some((None, rest))?
                }
                source => {
                    let (name, rest) = name(source)?;
                    match parenthesised(rest) {
                        // A function called for its rows is no table
                        Some((_, rest)) => (None, rest),
                        None => (Some(name), rest),
                    }
                }
            };
            let own = |alias: &String| (table.as_ref()).is_some_and(|table| table.eq_ignore_ascii_case(alias));
            sources.push(Source {
                alias: aliased(rest).filter(|alias| !own(alias)),
                table,
                outer,
            });
            continue;
        }
        if depth != 0 || named(from, at) {
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

/// The name `tokens` open with, unquoted and without its schema (`schema.table`), and what follows it.
fn name<'b, 't>(tokens: &'b [Leveled<'t>]) -> Option<(String, &'b [Leveled<'t>])> {
    let mut rest = tokens;
    loop {
        let [(_, name), after @ ..] = rest else {
            return None;
        };
        match after {
            _ if !is_word(name) => return None,
            [(_, "."), after @ ..] => rest = after,
            after => return Some((unquoted(name), after)),
        }
    }
}

/// The alias `tokens` open with, if they do: `AS alias`, or a bare word that is no keyword of a `FROM`.
fn aliased(tokens: &[Leveled]) -> Option<String> {
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
fn alias(tokens: &[Leveled]) -> bool {
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
