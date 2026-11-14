//! What a statement's result is where sqlc's report is not enough, read off
//! the statement's tokens. sqlc types a call by the function's first overload
//! without saying which function it called; reports a scalar subquery, and
//! every cast, NOT NULL, though a subquery without a row and an aggregate of
//! no rows are NULL; and reports every `sqlc.embed` nullable, joined or not.
//!
//! Each reader answers only what it can prove, and what it cannot read is the
//! answer that widens a type: a column that may be NULL, a statement that may
//! return no row, a table that may be missing. A wrong `X | None` is a
//! nuisance; a wrong `X` is a value that is not what its type says.
//!
//! A statement is read by its top level: the tokens outside every
//! parenthesis and bracket. A set operation (`UNION`, `INTERSECT`, `EXCEPT`)
//! makes a `SELECT` several branches, each read on its own, and a branch in
//! parentheses is read as what it holds. A word is a keyword only where it
//! can be one: after a `.` or an `AS` it is a name, whatever it spells.

use std::ops::Range;

#[cfg(test)]
#[path = "select_spec.rs"]
mod spec;

/// The aggregate functions of an engine, each of which makes the rows it
/// reads one, when the statement does not group them.
pub struct Aggregates {
    /// Those that are NULL of no rows: `sum`, `max`, ...
    pub nullable: &'static [&'static str],
    /// Those that never are NULL: `count`.
    pub never_null: &'static [&'static str],
    /// Those that are no aggregate of several arguments, but a function of
    /// each row: SQLite's `min` and `max`.
    pub scalar_of_several: &'static [&'static str],
    /// Whether a column may stand beside an aggregate that nothing groups,
    /// where it is NULL of no rows: on SQLite, and on MySQL without
    /// `ONLY_FULL_GROUP_BY`.
    pub bare_columns: bool,
}

/// One item of a result list: a column of the result, or one of an embed's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Item {
    /// Whether it is a bare `count(...)`, or a scalar subquery of one alone.
    pub count: bool,
    /// Whether it is NULL where sqlc reports it NOT NULL: of a scalar
    /// subquery that may find no row, or an aggregate of what may be no rows.
    pub nullable: bool,
}

/// What each of the `columns` items of the statement's result list is, by
/// position: the top-level `SELECT`'s list (after any `WITH`), or the
/// `RETURNING` list, split at its commas. In a set operation, an item is what
/// it is in every branch, and NULL if it may be in any. `None` where a list
/// is not read as one item per column: nothing is then known of any.
pub fn items(tokens: &[String], columns: usize, aggregates: &Aggregates) -> Option<Vec<Item>> {
    let mut read: Option<Vec<Item>> = None;
    for branch in branches(&leveled(tokens)) {
        let list = &branch[list(&branch)?];
        let split: Vec<&[Leveled]> = list.split(|&(d, t)| d == 0 && t == ",").collect();
        if list.is_empty() || split.len() != columns {
            return None;
        }
        let grouped = grouped(&branch);
        let bare = aggregates.bare_columns && aggregated(list, aggregates);
        let items = (split.iter()).map(|tokens| item(tokens, grouped, bare, aggregates));
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

/// Whether the statement returns exactly one row, whatever the data: an
/// `INSERT` of one row of `VALUES` (or `DEFAULT VALUES`) `RETURNING` it,
/// which no `OR IGNORE`, `ON CONFLICT DO NOTHING` or `DO UPDATE ... WHERE`
/// can skip; or a `SELECT` that aggregates its rows into one: its list calls
/// an aggregate, and nothing groups, filters or limits the result. A `SELECT`
/// of `EXISTS (...)` alone is one row too, of no table and unfiltered.
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
                    [(_, not), (_, exists), (_, "("), ..] => {
                        is(not, &["not"]) && is(exists, &["exists"])
                    }
                    _ => false,
                });
            // What follows a statement in parentheses limits it as its own
            // clauses do: `(SELECT count(*) FROM t) LIMIT 0`.
            collapses(&tokens) && collapses(branch) && (exists || aggregated(list, aggregates))
        }
        _ => false,
    }
}

/// The tables every row of the result has a row of, by name, as written: in
/// a `SELECT`, each one its `FROM` names outright or joins with an inner
/// join; in an `INSERT`, `UPDATE` or `DELETE` that returns its rows, the
/// table it writes, and those an `UPDATE`'s `FROM` so names.
///
/// A name proves nothing where it may be another's: a table an outer join
/// may leave without a row, an alias, a CTE. Nor does a statement whose
/// `FROM` is not read ([`sources`]), a set operation, or, where a column may
/// stand beside an ungrouped aggregate, one that aggregates no rows into a
/// row of NULLs.
pub fn present(tokens: &[String], aggregates: &Aggregates) -> Vec<String> {
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
    let tokens = leveled(tokens);
    let branches = branches(&tokens);
    let [branch] = branches.as_slice() else {
        return Vec::new();
    };
    let Some((at, kind)) = kind(branch) else {
        return Vec::new();
    };
    let statement = &branch[at..];
    let clause = |starts: &[&str], ends: &[&str]| {
        let clause = top(statement, starts).map_or(&[][..], |start| &statement[start + 1..]);
        &clause[..top(clause, ends).unwrap_or(clause.len())]
    };

    // What the statement writes, then what it reads beside it.
    let (target, read): (&[Leveled], &[Leveled]) = match kind {
        "select" => {
            let Some(list) = list(statement) else {
                return Vec::new();
            };
            let nulls = aggregates.bare_columns
                && !grouped(statement)
                && aggregated(&statement[list.clone()], aggregates);
            match &statement[list.end..] {
                [(_, from), rest @ ..] if is(from, &["from"]) && !nulls => {
                    (&[], &rest[..top(rest, ENDS).unwrap_or(rest.len())])
                }
                _ => return Vec::new(),
            }
        }
        _ if top(statement, &["returning"]).is_none() => return Vec::new(),
        "update" => (
            clause(&["update"], TARGET_ENDS),
            clause(&["from"], READ_ENDS),
        ),
        "delete" => (
            clause(&["from"], TARGET_ENDS),
            clause(&["using"], READ_ENDS),
        ),
        _ => (clause(&["into"], TARGET_ENDS), &[]),
    };

    // A CTE's name is no table's, whatever it spells: `WITH a AS (...)`.
    let mut missing: Vec<String> = (branch[..at].iter())
        .filter(|&&(d, t)| d == 0 && is_word(t))
        .map(|&(_, t)| unquoted(t))
        .collect();
    let mut proven = Vec::new();
    let read = [target, read]
        .into_iter()
        .filter(|sources| !sources.is_empty())
        .all(|sources| self::sources(sources, &mut proven, &mut missing).is_some());
    proven.retain(|name: &String| !missing.iter().any(|m| m.eq_ignore_ascii_case(name)));
    match read {
        true => proven,
        false => Vec::new(),
    }
}

/// A token of a statement's code, with the depth of parentheses and brackets
/// it stands at.
type Leveled<'t> = (usize, &'t str);

/// Each of `tokens` with its depth: a parenthesis or a bracket stands at the
/// depth it opens from, or closes to.
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

/// `tokens`, of inside parentheses, leveled from their own top.
fn lowered<'t>(tokens: &[Leveled<'t>]) -> Vec<Leveled<'t>> {
    let depth = tokens.first().map_or(0, |&(d, _)| d);
    (tokens.iter())
        .map(|&(d, t)| (d.saturating_sub(depth), t))
        .collect()
}

/// Whether `token` is one of `words`, in any case.
fn is(token: &str, words: &[&str]) -> bool {
    words.iter().any(|word| token.eq_ignore_ascii_case(word))
}

/// Whether the token at `at` is a name, whatever it spells: `k.group`,
/// `AS order`.
fn named(tokens: &[Leveled], at: usize) -> bool {
    at > 0 && is(tokens[at - 1].1, &[".", "as"])
}

/// Where the first of `words` stands at the top level of `tokens`, as a
/// keyword: no name, and not the `FROM` of `IS DISTINCT FROM` or the `GROUP`
/// of `WITHIN GROUP`.
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

/// What `tokens` hold in the parentheses they open with, and what follows
/// those.
fn parenthesised<'b, 't>(
    tokens: &'b [Leveled<'t>],
) -> Option<(&'b [Leveled<'t>], &'b [Leveled<'t>])> {
    let [(depth, "("), rest @ ..] = tokens else {
        return None;
    };
    let close = rest.iter().position(|&(d, t)| d == *depth && t == ")")?;
    Some((&rest[..close], &rest[close + 1..]))
}

/// What opens a statement in parentheses, which is then a subquery.
const QUERY: &[&str] = &["select", "with", "values"];

/// What makes a statement the branches of a set operation.
const SET_OPERATIONS: &[&str] = &["union", "intersect", "except"];

/// What may follow a statement in parentheses and still be its own.
const TAIL: &[&str] = &["order", "limit", "offset", "fetch"];

/// What ends a result list, and a `FROM` clause. Not `FOR`: MySQL's index
/// hints have one too (`USE INDEX FOR JOIN (...)`), and a `FOR UPDATE`,
/// which ends the statement, names no source.
const ENDS: &[&str] = &[
    "from", "into", "where", "group", "having", "window", "order", "limit", "offset", "fetch", ";",
];

/// The statement's branches, in no order: itself, or each side of a
/// `SELECT`'s set operations, one in parentheses as what it holds.
fn branches<'t>(tokens: &[Leveled<'t>]) -> Vec<Vec<Leveled<'t>>> {
    // An `INSERT ... SELECT ... UNION ...` is one statement, whose list is
    // its `RETURNING`.
    if kind(tokens).is_some_and(|(_, kind)| kind != "select") {
        return vec![tokens.to_vec()];
    }
    let mut branches = Vec::new();
    // Not recursion: a statement may nest its parentheses deeper than a
    // stack is.
    let mut held = vec![tokens.to_vec()];
    while let Some(tokens) = held.pop() {
        for branch in tokens.split(|&(d, t)| d == 0 && is(t, SET_OPERATIONS)) {
            let branch = match branch {
                [(_, quantifier), rest @ ..] if is(quantifier, &["all", "distinct"]) => rest,
                branch => branch,
            };
            // At most what orders or limits it follows the parentheses.
            let inner = parenthesised(branch)
                .filter(|(_, after)| (after.first()).is_none_or(|&(_, t)| t == ";" || is(t, TAIL)));
            match inner {
                Some((inner, _)) => held.push(lowered(inner)),
                None => branches.push(branch.to_vec()),
            }
        }
    }
    branches
}

/// What the statement is, and where it says so: its first top-level
/// `SELECT`, `INSERT`, `REPLACE`, `UPDATE` or `DELETE`, after any `WITH`.
fn kind(tokens: &[Leveled]) -> Option<(usize, &'static str)> {
    const KINDS: [&str; 5] = ["select", "insert", "replace", "update", "delete"];
    let at = top(tokens, &KINDS)?;
    let kind = KINDS.into_iter().find(|kind| is(tokens[at].1, &[kind]))?;
    Some((at, kind))
}

/// Where the statement's result list is: after `SELECT` and any `DISTINCT`,
/// up to the clause that ends it; or after `RETURNING`.
fn list(tokens: &[Leveled]) -> Option<Range<usize>> {
    let (at, kind) = kind(tokens)?;
    if kind != "select" {
        let start = top(tokens, &["returning"])? + 1;
        let end = top(&tokens[start..], &[";"]).map_or(tokens.len(), |end| start + end);
        return Some(start..end);
    }
    let mut start = at + 1;
    if tokens
        .get(start)
        .is_some_and(|&(_, t)| is(t, &["distinct", "all"]))
    {
        start += 1;
        // `DISTINCT ON (...)`.
        if tokens.get(start).is_some_and(|&(_, t)| is(t, &["on"])) {
            start += top(&tokens[start..], &[")"])? + 1;
        }
    }
    let end = top(&tokens[start..], ENDS).map_or(tokens.len(), |end| start + end);
    Some(start..end)
}

/// Whether the statement, if its list aggregates, returns the one row that
/// makes: nothing groups its rows, filters the groups, or limits the result.
fn collapses(tokens: &[Leveled]) -> bool {
    top(
        tokens,
        &["group", "having", "window", "limit", "offset", "fetch"],
    )
    .is_none()
}

/// Whether the statement groups its rows, so that what it aggregates is a
/// row at least. Not into the empty grouping set, which is a row of no rows:
/// `GROUP BY ()`, and what a `ROLLUP`, a `CUBE` or `GROUPING SETS` may hold.
fn grouped(tokens: &[Leveled]) -> bool {
    top(tokens, &["group"]).is_some_and(|at| {
        let grouping = &tokens[at..];
        !matches!(grouping, [_, _, (_, "("), (_, ")"), ..])
            && !(grouping.iter()).any(|&(_, t)| is(t, &["rollup", "cube", "sets"]))
    })
}

/// What may be NULL of no rows, as [`found`] in an item.
struct Found {
    /// Whether it is an aggregate's call, not a scalar subquery.
    aggregate: bool,
    /// Whether it is NULL of no rows: an aggregate but a `count`, a subquery
    /// but one that counts all its rows.
    nullable: bool,
    /// Whether it is inside a `coalesce` that ends in a default.
    guarded: bool,
}

/// Each aggregate `tokens` call, and each scalar subquery they hold, outside
/// every subquery.
///
/// A call with `OVER` is a window function's, of each row, and one of a
/// schema but `pg_catalog` that schema's own function. A subquery that is no
/// value is none: one `EXISTS`, `IN`, `ANY` and their like read, the
/// elements of an `ARRAY(...)`, and one compared with `IS`.
fn found(tokens: &[Leveled], aggregates: &Aggregates) -> Vec<Found> {
    let mut found = Vec::new();
    // Whether each parenthesis open here is a `coalesce` with a default.
    let mut guards: Vec<bool> = Vec::new();
    let mut at = 0;
    while at < tokens.len() {
        let (_, token) = tokens[at];
        let before = at.checked_sub(1).map_or("", |before| tokens[before].1);
        match (token, parenthesised(&tokens[at..])) {
            ("(", Some((inner, after))) if inner.first().is_some_and(|&(_, t)| is(t, QUERY)) => {
                let after = after.first().map_or("", |&(_, t)| t);
                let value = !is(before, &["exists", "in", "any", "all", "some", "array"])
                    && !is(after, &["is"]);
                // Limited from outside its parentheses, a count may have no
                // row: `((SELECT count(*) FROM t) LIMIT 0)`.
                let counts = !is(after, SET_OPERATIONS)
                    && !is(after, TAIL)
                    && selects(inner).is_some_and(|(call, collapses)| {
                        collapses && is(&call, aggregates.never_null)
                    });
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
                guards.push(default && is(before, &["coalesce", "ifnull"]));
            }
            (")", _) => {
                guards.pop();
            }
            (name, _) => {
                let name = unquoted(name);
                let nullable = is(&name, aggregates.nullable);
                // Another schema's function of an aggregate's name is its own.
                let schema = at.checked_sub(2).filter(|_| before == ".");
                let builtin = schema.is_none_or(|schema| is(tokens[schema].1, &["pg_catalog"]));
                if (nullable || is(&name, aggregates.never_null))
                    && builtin
                    && let Some((arguments, after)) = parenthesised(&tokens[at + 1..])
                    && !windowed(after)
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

/// Whether `tokens` call an aggregate, which makes the rows of a statement
/// that does not group them one.
fn aggregated(tokens: &[Leveled], aggregates: &Aggregates) -> bool {
    found(tokens, aggregates)
        .iter()
        .any(|found| found.aggregate)
}

/// Whether what follows a call makes it a window function's: `OVER`, after
/// any `FILTER (...)`.
fn windowed(after: &[Leveled]) -> bool {
    let after = match after {
        [(_, filter), rest @ ..] if is(filter, &["filter"]) => {
            parenthesised(rest).map_or(after, |(_, after)| after)
        }
        after => after,
    };
    after.first().is_some_and(|&(_, t)| is(t, &["over"]))
}

/// Whether a call's `arguments` are several.
fn several(arguments: &[Leveled]) -> bool {
    let depth = arguments.first().map_or(0, |&(d, _)| d);
    arguments.iter().any(|&(d, t)| d == depth && t == ",")
}

/// Whether the last of a `coalesce`'s `arguments` is a default: a literal,
/// cast or not, which is never NULL. Anything else may be: `coalesce(max(x),
/// max(y))` is NULL of no rows.
fn defaults(arguments: &[Leveled]) -> bool {
    let depth = arguments.first().map_or(0, |&(d, _)| d);
    let last = (arguments.rsplit(|&(d, t)| d == depth && t == ","))
        .next()
        .unwrap_or_default();
    let (literal, cast) = match last {
        [(_, "-" | "+"), (_, number), cast @ ..] => (number, cast),
        [(_, literal), cast @ ..] => (literal, cast),
        [] => return false,
    };
    let literal = literal.starts_with(|c: char| c.is_ascii_digit() || c == '\'')
        || is(literal, &["true", "false"]);
    // A cast to one word and its modifiers, `::numeric(12, 2)`: more words
    // could be an operator's (`0::int OR x`).
    let cast = match cast {
        [] => true,
        [(_, cast), (_, r#type), modifiers @ ..] => {
            cast.starts_with(':')
                && is_word(r#type)
                && (modifiers.iter()).all(|&(_, t)| {
                    t.starts_with(|c: char| c.is_ascii_digit() || "()[],".contains(c))
                })
        }
        _ => false,
    };
    literal && cast
}

/// What `tokens`, one item of a result list, are: `grouped`, in a statement
/// whose groups each have a row; `bare`, beside an aggregate where a column
/// may stand ungrouped, NULL of no rows.
fn item(tokens: &[Leveled], grouped: bool, bare: bool, aggregates: &Aggregates) -> Item {
    let found = found(tokens, aggregates);
    let count = match parenthesised(tokens) {
        Some((inner, after)) if alias(after) => {
            selects(inner).is_some_and(|(call, _)| call == "count")
        }
        _ => call(tokens).is_some_and(|call| call == "count"),
    };
    let null = |found: &Found| found.nullable && !found.guarded && !(found.aggregate && grouped);
    let column = bare && !grouped && !found.iter().any(|found| found.aggregate);
    Item {
        count,
        nullable: column || found.iter().any(null),
    }
}

/// The function `tokens` are a bare call of, lowercase: a name and its
/// parenthesised arguments, with at most an alias after them.
fn call(tokens: &[Leveled]) -> Option<String> {
    let [(_, name), rest @ ..] = tokens else {
        return None;
    };
    let (_, after) = parenthesised(rest)?;
    let bare = is_word(name) && !name.starts_with(['"', '`', '[']);
    (bare && alias(after)).then(|| name.to_lowercase())
}

/// The function the subquery `inner` selects a bare call of, alone, and
/// whether nothing groups, filters or limits its rows: of a `count`, it then
/// always has its one row.
fn selects(inner: &[Leveled]) -> Option<(String, bool)> {
    let inner = lowered(inner);
    let [branch] = <[_; 1]>::try_from(branches(&inner)).ok()?;
    let call = call(&branch[list(&branch)?])?;
    Some((call, collapses(&inner) && collapses(&branch)))
}

/// Whether an `INSERT`, from its `INSERT` on, returns the one row it inserts.
fn inserts_one(tokens: &[Leveled]) -> bool {
    let Some(returning) = top(tokens, &["returning"]) else {
        return false;
    };
    // A column of the list returned may be named `values` or `do`.
    let head = &tokens[..returning];
    let Some(values) = top(head, &["values"]) else {
        return false;
    };
    let row = &head[values + 1..];
    let row = &row[..top(row, &["on"]).unwrap_or(row.len())];
    let rows = row.iter().filter(|&&(d, t)| d == 0 && t == "(").count();
    let defaults = values > 0 && is(head[values - 1].1, &["default"]);

    // What may leave the row out: SQLite's `INSERT OR IGNORE`, `DO NOTHING`,
    // and the `WHERE` of a `DO UPDATE`.
    let ignores =
        matches!(head, [_, (_, or), (_, ignore), ..] if is(or, &["or"]) && is(ignore, &["ignore"]));
    let skips = top(head, &["do"])
        .is_some_and(|action| top(&head[action..], &["nothing", "where"]).is_some());
    (rows == 1 || (defaults && rows == 0)) && !ignores && !skips
}

/// Notes each table the sources of a `FROM` clause name: in `proven` if
/// every row has a row of it, in `missing` if an outer join may leave it
/// without one. Every alias is `missing` too: it is no table's name, though
/// it may spell one (`LEFT JOIN c AS b`, a subquery `AS b`).
///
/// `None` where the clause is not read: with a `RIGHT` or `FULL` join, a
/// join in parentheses, or a join inside another's, which starts before the
/// other's `ON` (`a LEFT JOIN b JOIN c ON ... ON ...`).
fn sources(from: &[Leveled], proven: &mut Vec<String>, missing: &mut Vec<String>) -> Option<()> {
    const SIDES: &[&str] = &[
        "natural", "inner", "cross", "left", "right", "full", "outer",
    ];
    // Where the next source starts, and whether an outer join brings it.
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
                    // A subquery; anything else is a join in parentheses.
                    inner.first().filter(|&&(_, t)| is(t, QUERY))?;
                    (None, rest)
                }
                source => {
                    let (name, rest) = name(source)?;
                    match parenthesised(rest) {
                        // A function called for its rows is no table.
                        Some((_, rest)) => (None, rest),
                        None => (Some(name), rest),
                    }
                }
            };
            let own = |alias: &String| {
                (table.as_ref()).is_some_and(|table| table.eq_ignore_ascii_case(alias))
            };
            missing.extend(aliased(rest).filter(|alias| !own(alias)));
            match (table, outer) {
                (Some(table), false) => proven.push(table),
                (Some(table), true) => missing.push(table),
                (None, _) => {}
            }
            continue;
        }
        if depth != 0 || named(from, at) {
            continue;
        }
        if token == "," {
            next = Some((at + 1, false));
        } else if is(token, &["on", "using"]) {
            awaits_on = false;
        } else if is(token, &["join", "straight_join"]) {
            let sides: Vec<&str> = (from[..at].iter().rev())
                .take_while(|&&(d, t)| d == 0 && is(t, SIDES))
                .map(|&(_, t)| t)
                .collect();
            let side = |side: &str| sides.iter().any(|s| is(s, &[side]));
            if awaits_on || side("right") || side("full") {
                return None;
            }
            awaits_on = !side("cross") && !side("natural");
            next = Some((at + 1, side("left")));
        }
    }
    Some(())
}

/// The name `tokens` open with, unquoted and without its schema
/// (`schema.table`), and what follows it.
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

/// The alias `tokens` open with, if they do: `AS alias`, or a bare word that
/// is no keyword of a `FROM`.
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

/// `name` without the quotes of a quoted identifier.
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

/// Whether `token` is a word, quoted or not: a name or an alias.
fn is_word(token: &str) -> bool {
    token.starts_with(|c: char| c.is_alphabetic() || "_\"`[".contains(c))
}
