//! sqlc reports the first arm of a UNION and loses the expression behind a derived column. We follow the
//! visible projections so those boundaries cannot turn a guessed type into an apparently declared one.

use std::collections::BTreeMap;
use std::rc::Rc;

use super::*;
use crate::plugin;

const DERIVED: (&str, &str) = (
    "comes from a derived expression whose type sqlc cannot establish",
    "cast the expression in its SELECT",
);
pub(super) const UNION: (&str, &str) = (
    "has set-operation branches without one verified SQL type",
    "cast this column to the same type in every branch",
);

#[derive(Clone, Default)]
struct Field {
    name: String,
    ty: Option<String>,
    item: Item,
}

type Relations = BTreeMap<String, Option<Rc<Vec<Field>>>>;

struct Read<'a> {
    engine: Engine,
    catalog: &'a plugin::Catalog,
    tables: &'a [&'a str],
}

pub(super) fn read(
    tokens: &[Leveled],
    engine: Engine,
    catalog: &plugin::Catalog,
    tables: &[&str],
) -> Option<Vec<Item>> {
    let fields = Read {
        engine,
        catalog,
        tables,
    }
    .query(tokens, &Relations::new(), 0)?;
    (fields.len() == tables.len()).then(|| fields.into_iter().map(|field| field.item).collect())
}

impl Read<'_> {
    fn query(&self, tokens: &[Leveled], inherited: &Relations, depth: usize) -> Option<Vec<Field>> {
        // A recursive CTE can refer to itself. We only follow definitions already read at this level.
        if depth >= 48 {
            return None;
        }
        let mut relations = inherited.clone();
        let mut rest = tokens;
        if let [(_, with), tail @ ..] = rest
            && is(with, &["with"])
        {
            let recursive = tail.first().is_some_and(|t| is(t.1, &["recursive"]));
            rest = match tail {
                [(_, word), tail @ ..] if is(word, &["recursive"]) => tail,
                tail => tail,
            };
            loop {
                let [(_, name), after @ ..] = rest else { return None };
                let name = unquoted(name);
                let (names, after) = parenthesised(after).map_or((None, after), |(names, rest)| (Some(names), rest));
                let [(_, keyword), remaining @ ..] = after else {
                    return None;
                };
                if !is(keyword, &["as"]) {
                    return None;
                }
                let mut after = remaining;
                while let [(_, word), tail @ ..] = after
                    && is(word, &["not", "materialized"])
                {
                    after = tail;
                }
                let (body, after) = parenthesised(after)?;
                relations.insert(name.clone(), None);
                let body = lowered(body);
                let anchor = recursive.then(|| self.recursive_anchor(&body, &name)).flatten();
                let fields = match anchor {
                    Some(anchor) => self.anchored(anchor, &relations, depth),
                    None => self.query(&body, &relations, depth + 1),
                };
                relations.insert(name, renamed(fields, names).map(Rc::new));
                match after {
                    [(_, ","), tail @ ..] => rest = tail,
                    tail => {
                        rest = tail;
                        break;
                    }
                }
            }
        }

        let mut output: Option<Vec<Field>> = None;
        let aggregates = aggregates(self.engine);
        for tokens in branches(rest) {
            let branch = Branch::read(&tokens, &aggregates)?;
            if depth == 0 && branch.expressions().count() != self.tables.len() {
                return None;
            }
            let mut local = relations.clone();
            // Derived tables expose the names of their own SELECT at the alias in the surrounding FROM.
            for (at, &(level, token)) in branch.tail.iter().enumerate() {
                if level != 0
                    || token != "("
                    || !at
                        .checked_sub(1)
                        .is_some_and(|i| is(branch.tail[i].1, &["from", "join", "lateral", ","]))
                {
                    continue;
                }
                if let Some((body, after)) = parenthesised(&branch.tail[at..])
                    && is_statement(body)
                    && let Some(alias) = leading_alias(after)
                {
                    let skip = if is(after[0].1, &["as"]) { 2 } else { 1 };
                    let names = after.get(skip..).and_then(parenthesised).map(|(names, _)| names);
                    let fields = self.query(&lowered(body), &local, depth + 1);
                    local.insert(alias, renamed(fields, names).map(Rc::new));
                }
            }
            let sources = branch.sources.as_ref().map(|(_, sources)| sources.as_slice());
            // A column we cannot place among derived rows, or in a `FROM` we did not read, may be any of theirs
            let derives = |sources: &[Source]| sources.iter().any(|source| self.derived(source, &local).is_some());
            let lookup = |tokens: &[Leveled]| {
                self.field(tokens, sources, &local).or_else(|| {
                    let name = unquoted(column(tokens)?.last()?);
                    sources.is_none_or(derives).then(|| Field {
                        name,
                        item: Item::nullable(true),
                        ..Field::default()
                    })
                })
            };
            let mut fields = Vec::new();
            for (position, tokens) in branch.expressions().enumerate() {
                if tokens.is_empty() {
                    return None;
                }
                let (expression, alias) = aliased(tokens);
                if let [(_, "*")] | [(_, _), (_, "."), (_, "*")] = expression {
                    let qualifier = (expression.len() == 3).then(|| unquoted(expression[0].1));
                    for source in sources? {
                        let exposed = source.alias.as_ref().or(source.table.as_ref());
                        if qualifier.is_some() && qualifier.as_ref() != exposed {
                            continue;
                        }
                        fields.extend(self.all(source, &local)?);
                    }
                    continue;
                }
                let mut field = self.expression(expression, &lookup, depth);
                if let Some(name) = alias {
                    field.name = unquoted(name);
                }
                let required = |column: &[Leveled]| lookup(column).map(|f| !f.item.nullable);
                let table = if depth == 0 { self.tables[position] } else { "" };
                let facts = branch.item(expression, table, &aggregates, required);
                field.item = Item {
                    nullable: field.item.nullable || facts.nullable,
                    required_elements: field.item.required_elements || facts.required_elements,
                    problem: field.item.problem.or(facts.problem),
                    ..facts
                };
                if self.engine == Engine::PostgreSql
                    && let Some(rows) = array_rows(expression)
                    && let Some(fields) = self.query(&lowered(rows), &local, depth + 1)
                    && matches!(&fields[..], [f] if f.ty.is_some() && !f.item.nullable && f.item.problem.is_none())
                {
                    field.item.required_elements = true;
                }
                if depth > 0 && field.ty.is_none() {
                    field.item.problem = field.item.problem.or(Some(DERIVED));
                }
                fields.push(field);
            }
            match &mut output {
                None => output = Some(fields),
                Some(first) if first.len() == fields.len() => {
                    for (left, right) in first.iter_mut().zip(fields) {
                        if left.ty.is_none() || left.ty != right.ty {
                            left.item.problem = Some(UNION);
                            left.ty = None;
                        }
                        left.item = left.item.union(right.item);
                    }
                }
                Some(_) => return None,
            }
        }
        output
    }

    /// The fields of a recursive CTE, read off its nonrecursive term `anchor`, in parentheses as it may be. `None`
    /// where we cannot read it, as for a `VALUES`.
    fn anchored(&self, anchor: &[Leveled], relations: &Relations, depth: usize) -> Option<Vec<Field>> {
        let [anchor] = <[_; 1]>::try_from(branches(anchor)).ok()?;
        let mut fields = self.query(&anchor, relations, depth + 1)?;
        let items = &anchor[list(&anchor)?];
        let expressions = items.split(|&(d, t)| d == 0 && t == ",");
        if expressions.clone().count() != fields.len() {
            return None;
        }
        for (field, expression) in fields.iter_mut().zip(expressions) {
            if matches!(aliased(expression).0, [(_, value)] if is(value, &["null"])) {
                field.ty = Some("text".into());
                field.item.problem = None;
            }
            field.item.nullable = true;
            field.item.required_elements = false;
        }
        Some(fields)
    }

    /// PostgreSQL fixes a recursive CTE's types from its nonrecursive term. An untyped NULL in it is
    /// text (parse_cte.c, analyzeCTETargetList). A later iteration may supply NULL to any column.
    fn recursive_anchor<'a, 't>(&self, tokens: &'a [Leveled<'t>], name: &str) -> Option<&'a [Leveled<'t>]> {
        if self.engine != Engine::PostgreSql {
            return None;
        }
        let at = top(tokens, &["union"])?;
        let rhs = match &tokens[at + 1..] {
            [(_, word), tail @ ..] if is(word, &["all", "distinct"]) => tail,
            tail => tail,
        };
        if top(rhs, SET_OPERATIONS).is_some() || !rhs.first().is_some_and(|t| is(t.1, &["select"])) {
            return None;
        }
        let (_, sources) = Branch::read(rhs, &aggregates(self.engine))?.sources?;
        sources
            .iter()
            .any(|s| s.schema.is_none() && s.table.as_deref() == Some(name))
            .then_some(&tokens[..at])
    }

    fn declared(&self, column: &plugin::Column) -> Field {
        let mut ty = self.engine.type_name(&column.data_type());
        if self.engine == Engine::MySql && ty == "tinyint" && column.length == 1 {
            ty.push_str("(1)");
        }
        let unsigned = if column.unsigned { " unsigned" } else { "" };
        Field {
            name: column.name.clone(),
            ty: (!ty.is_empty()).then(|| {
                format!(
                    "{ty}{unsigned}{}",
                    "[]".repeat(usize::try_from(column.array_dims).unwrap_or(0).min(6))
                )
            }),
            item: Item::nullable(!column.not_null),
        }
    }

    fn derived<'a>(&self, source: &Source, relations: &'a Relations) -> Option<&'a Option<Rc<Vec<Field>>>> {
        if source.schema.is_some() {
            return None;
        }
        relations.get(source.table.as_ref().or(source.alias.as_ref())?)
    }

    fn table(&self, source: &Source) -> Option<&plugin::Table> {
        let table = source.table.as_ref()?;
        self.catalog.table(source.schema.as_deref().unwrap_or(""), table)
    }

    fn all(&self, source: &Source, relations: &Relations) -> Option<Vec<Field>> {
        let mut fields: Vec<Field> = match self.derived(source, relations) {
            Some(derived) => derived.as_ref()?.as_ref().clone(),
            None => self.table(source)?.columns.iter().map(|c| self.declared(c)).collect(),
        };
        for field in &mut fields {
            field.item.nullable |= source.outer;
        }
        Some(fields)
    }

    fn field(&self, tokens: &[Leveled], sources: Option<&[Source]>, relations: &Relations) -> Option<Field> {
        let parts = column(tokens)?;
        let name = unquoted(parts.last()?);
        let qualifier = parts.iter().rev().nth(1).map(|s| unquoted(s));
        let mut found = None;
        for source in sources? {
            if qualifier.is_some() && qualifier.as_ref() != source.alias.as_ref().or(source.table.as_ref()) {
                continue;
            }
            let candidate = match self.derived(source, relations) {
                Some(Some(fields)) => fields.iter().find(|f| f.name == name).cloned(),
                // A relation we could not read may well have the column
                Some(None) => Some(Field {
                    name: name.clone(),
                    item: Item {
                        nullable: true,
                        problem: Some(DERIVED),
                        ..Item::default()
                    },
                    ..Field::default()
                }),
                None => self
                    .table(source)
                    .and_then(|t| t.columns.iter().find(|c| c.name == name))
                    .map(|c| self.declared(c)),
            };
            if let Some(mut field) = candidate {
                if found.is_some() {
                    return None;
                }
                field.item.nullable |= source.outer;
                found = Some(field);
            }
        }
        found
    }

    fn expression(&self, tokens: &[Leveled], lookup: &impl Fn(&[Leveled]) -> Option<Field>, depth: usize) -> Field {
        if depth >= 48 || tokens.is_empty() {
            return Field {
                item: Item::nullable(true),
                ..Field::default()
            };
        }
        let recurse = |tokens: &[Leveled]| self.expression(tokens, lookup, depth + 1);
        let aggregates = aggregates(self.engine);
        if let Some((body, [])) = parenthesised(tokens) {
            if !is_statement(body) {
                return recurse(body);
            }
            // A scalar subquery is NULL without a row, which only a count of every row always has
            let counts =
                selects(body, &aggregates).is_some_and(|(call, whole)| whole && is(&call, aggregates.never_null));
            return Field {
                item: Item::nullable(!counts),
                ..Field::default()
            };
        }
        // A sign keeps what it stands before
        if let [(_, "-" | "+"), signed @ ..] = tokens
            && !signed.is_empty()
        {
            return recurse(signed);
        }
        let level = tokens[0].0;
        if is_literal_case(tokens) {
            let values = case_literals(tokens).unwrap_or_default();
            let ty = if values.iter().all(|v| v.starts_with('\'')) {
                "text"
            } else {
                "bool"
            };
            return Field {
                ty: Some(ty.into()),
                ..Field::default()
            };
        }
        if is(tokens[0].1, &["case"]) && after_case(tokens).is_some_and(|rest| rest.is_empty()) {
            // With an `ELSE` and a literal in every branch, none of them `NULL`, it always has a value
            let literal = |value: &&str| value.starts_with('\'') || value.parse::<i32>().is_ok();
            let valued = case_literals(tokens).is_some_and(|values| values.iter().all(literal));
            return Field {
                item: Item::nullable(!valued),
                ..Field::default()
            };
        }
        if let Some(at) = tokens
            .iter()
            .position(|&(d, t)| d == level && is(t, &["and", "or", "like", "ilike", "in", "between"]))
        {
            let nullable = is(tokens[at].1, &["in", "between"])
                || recurse(&tokens[..at]).item.nullable
                || recurse(&tokens[at + 1..]).item.nullable;
            return Field {
                ty: Some("bool".into()),
                item: Item::nullable(nullable),
                ..Field::default()
            };
        }
        if is(tokens[0].1, &["not"]) {
            return Field {
                item: Item::nullable(recurse(&tokens[1..]).item.nullable),
                ty: Some("bool".into()),
                ..Field::default()
            };
        }
        if let Some(at) = tokens.iter().position(|&(d, t)| d == level && is(t, &["is"])) {
            let tested = &tokens[at + 1..];
            let truth = match tested {
                [(_, not), truth @ ..] if is(not, &["not"]) => truth,
                truth => truth,
            };
            if matches!(truth, [(_, truth)] if is(truth, &["null", "true", "false", "unknown"])) {
                return Field {
                    ty: Some("bool".into()),
                    ..Field::default()
                };
            }
        }
        if let Some(at) = tokens.iter().position(|&(d, t)| {
            d == level
                && matches!(
                    t,
                    "+" | "-" | "*" | "/" | "%" | "||" | "=" | "<>" | "!=" | "<" | ">" | "<=" | ">=" | "!"
                )
        }) && at > 0
        {
            let op = tokens[at].1;
            // SQLite's and MySQL's lexers read `>=`, `<>` and `!=` as two tokens
            let right = match &tokens[at + 1..] {
                [(_, "=" | ">"), right @ ..] if matches!(op, "<" | ">" | "!") => right,
                right => right,
            };
            let left = recurse(&tokens[..at]);
            let right = recurse(right);
            let nullable = left.item.nullable
                || right.item.nullable
                || (self.engine != Engine::PostgreSql && matches!(op, "/" | "%"));
            return Field {
                item: Item::nullable(nullable),
                ty: matches!(op, "=" | "<>" | "!=" | "<" | ">" | "<=" | ">=" | "!").then(|| "bool".into()),
                ..Field::default()
            };
        }
        if let Some((body, ty)) = cast(tokens) {
            let value = recurse(body);
            // MySQL casts what reads as no date or time to NULL with a warning, where every other cast of a value is
            // an error or a value
            const TIMES: &[&str] = &["date", "datetime", "time", "timestamp", "year"];
            let is_time = |ty: &str| is(ty.split([' ', '(']).next().unwrap_or_default(), TIMES);
            let unread = self.engine == Engine::MySql && is_time(&ty) && !value.ty.as_deref().is_some_and(is_time);
            return Field {
                ty: Some(self.engine.type_name(&ty)),
                item: Item {
                    problem: None,
                    nullable: value.item.nullable || unread,
                    ..value.item
                },
                ..value
            };
        }
        if let Some(field) = lookup(tokens) {
            return field;
        }
        // A number is never NULL: `1.5` is one token to PostgreSQL and three to SQLite and MySQL
        let digits = |token: &str| token.starts_with(|c: char| c.is_ascii_digit());
        if let [(_, number)] | [(_, number), (_, "."), _] = tokens
            && digits(number)
            && (tokens.len() > 1 || number.parse::<i32>().is_err())
        {
            return Field::default();
        }
        if let [(_, value)] = tokens {
            let ty = if value.parse::<i32>().is_ok() {
                Some(self.engine.type_name("int"))
            } else if value.starts_with('\'') {
                Some("text".into())
            } else if is(value, &["true", "false"]) {
                Some("bool".into())
            } else {
                None
            };
            return Field {
                item: Item::nullable(
                    (depth > 0 && ty.is_none()) || is(value, &["null"]) || value.starts_with('$') || *value == "?",
                ),
                ty,
                name: unquoted(value),
            };
        }
        if let [(_, name), after @ ..] = tokens
            && let Some((args, tail)) = parenthesised(after)
        {
            if is(name, &["count"]) && over(tail).1.is_empty() {
                return Field {
                    ty: Some(
                        self.engine
                            .type_name(crate::sqlc_typing::inference::count_type(self.engine)),
                    ),
                    name: "count".into(),
                    ..Field::default()
                };
            }
            if is(name, &["coalesce", "ifnull"]) && tail.is_empty() {
                let nullable = args
                    .split(|&(d, t)| d == level + 1 && t == ",")
                    .all(|arg| recurse(arg).item.nullable);
                return Field {
                    name: unquoted(name),
                    item: Item::nullable(nullable),
                    ..Field::default()
                };
            }
        }
        // The existing aggregate and CASE rules know which of their rows can supply NULL. SQLite's `max(a, b)` is no
        // aggregate but a function of each row, which is NULL when an argument is.
        let scalar = is(tokens[0].1, aggregates.scalar_of_several)
            && parenthesised(&tokens[1..]).is_some_and(|(arguments, _)| several(arguments));
        let aggregate = |(name, _): (String, bool)| is(&name, aggregates.nullable) || is(&name, aggregates.never_null);
        let recognized = (!scalar && call(tokens).is_some_and(aggregate))
            || (is(tokens[0].1, &["case"]) && case_literals(tokens).is_some())
            // An `EXISTS (...)`, an `ARRAY[...]` or an `ARRAY(...)` alone: an operator after it may still be NULL
            || (is(tokens[0].1, &["exists", "array"]) && closes(&tokens[1..]));
        Field {
            item: Item::nullable(!recognized && (depth > 0 || column(tokens).is_none())),
            ..Field::default()
        }
    }
}

/// Whether `tokens` are one pair of parentheses or brackets and what it holds.
fn closes(tokens: &[Leveled]) -> bool {
    let [(depth, "(" | "["), held @ ..] = tokens else {
        return false;
    };
    held.iter().position(|&(d, t)| d == *depth && matches!(t, ")" | "]")) == Some(held.len() - 1)
}

fn array_rows<'a, 't>(mut tokens: &'a [Leveled<'t>]) -> Option<&'a [Leveled<'t>]> {
    if let Some((inner, _)) = cast(tokens) {
        tokens = inner;
    }
    let [(_, array), rest @ ..] = tokens else { return None };
    let (rows, []) = parenthesised(rest)? else { return None };
    (is(array, &["array"]) && is_statement(rows)).then_some(rows)
}

fn renamed(mut fields: Option<Vec<Field>>, names: Option<&[Leveled]>) -> Option<Vec<Field>> {
    if let Some(names) = names {
        for (field, name) in fields.as_mut()?.iter_mut().zip(names.split(|&(_, t)| t == ",")) {
            let [(_, name)] = name else { return None };
            field.name = unquoted(name);
        }
    }
    fields
}

fn aliased<'a, 't>(tokens: &'a [Leveled<'t>]) -> (&'a [Leveled<'t>], Option<&'t str>) {
    let level = tokens.first().map_or(0, |t| t.0);
    match tokens {
        [expression @ .., (d, keyword), (_, name)] if *d == level && is(keyword, &["as"]) => (expression, Some(name)),
        [expression @ .., (_, alias)]
            if !expression.is_empty()
                && is_word(alias)
                && !is(alias, &["end", "null", "true", "false"])
                && (expression.last().is_some_and(|t| t.1 == ")")
                    || column(tokens).is_some_and(|c| c.last() != Some(alias))) =>
        {
            (expression, Some(alias))
        }
        _ => (tokens, None),
    }
}

fn cast<'a, 't>(tokens: &'a [Leveled<'t>]) -> Option<(&'a [Leveled<'t>], String)> {
    let (operand, ty) = match tokens {
        [(_, word), tail @ ..] if is(word, &["cast"]) => {
            let (inside, []) = parenthesised(tail)? else {
                return None;
            };
            let at = inside
                .iter()
                .rposition(|&(d, t)| d == tokens[0].0 + 1 && is(t, &["as"]))?;
            (&inside[..at], &inside[at + 1..])
        }
        _ => {
            let at = tokens
                .windows(2)
                .rposition(|w| w[0].0 == tokens[0].0 && w[0].1 == ":" && w[1].1 == ":")?;
            (&tokens[..at], &tokens[at + 2..])
        }
    };
    if ty.is_empty()
        || !ty.iter().all(|t| {
            is_word(t.1) || t.1.bytes().all(|c| c.is_ascii_digit()) || matches!(t.1, "." | "(" | ")" | "," | "[" | "]")
        })
    {
        return None;
    }
    Some((operand, ty.iter().map(|t| t.1).collect::<Vec<_>>().join(" ")))
}
