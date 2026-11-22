//! sqlc reports the first arm of a UNION and loses the expression behind a derived column. We follow the
//! visible projections so those boundaries cannot turn a guessed type into an apparently declared one.

use std::collections::BTreeMap;
use std::rc::Rc;

use super::*;
use crate::plugin;

const DERIVED: &str =
    "comes from a derived expression whose type sqlc cannot establish; cast the expression in its SELECT";
const UNION: &str =
    "has set-operation branches without one verified SQL type; cast this column to the same type in every branch";

#[derive(Clone, Default)]
struct Field {
    name: String,
    ty: Option<String>,
    nullable: bool,
    problem: Option<&'static str>,
    required_elements: bool,
}

type Relations = BTreeMap<String, Option<Rc<Vec<Field>>>>;

struct Read<'a> {
    engine: Engine,
    catalog: &'a plugin::Catalog,
}

pub(super) fn apply(tokens: &[Leveled], engine: Engine, catalog: &plugin::Catalog, items: &mut [Item]) {
    let read = Read { engine, catalog };
    if let Some(fields) = read.query(tokens, &Relations::new(), 0)
        && fields.len() == items.len()
    {
        for (item, field) in items.iter_mut().zip(fields) {
            item.nullable |= field.nullable;
            item.problem = field.problem;
            item.required_elements |= field.required_elements;
        }
    } else if branches(tokens).len() > 1 {
        for item in items {
            item.problem = Some(UNION);
        }
    }
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
                let fields = if recursive && let Some(anchor) = self.recursive_anchor(&body, &name) {
                    let mut fields = self.query(anchor, &relations, depth + 1)?;
                    let items = &anchor[list(anchor)?];
                    let expressions = items.split(|&(d, t)| d == 0 && t == ",");
                    if expressions.clone().count() != fields.len() {
                        return None;
                    }
                    for (field, expression) in fields.iter_mut().zip(expressions) {
                        if matches!(aliased(expression).0, [(_, value)] if is(value, &["null"])) {
                            field.ty = Some("text".into());
                            field.problem = None;
                        }
                        field.nullable = true;
                        field.required_elements = false;
                    }
                    Some(fields)
                } else {
                    self.query(&body, &relations, depth + 1)
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
        for branch in branches(rest) {
            let range = list(&branch)?;
            let mut local = relations.clone();
            // Derived tables expose the names of their own SELECT at the alias in the surrounding FROM.
            for (at, &(level, token)) in branch.iter().enumerate().skip(range.end) {
                if level != 0
                    || token != "("
                    || !at
                        .checked_sub(1)
                        .is_some_and(|i| is(branch[i].1, &["from", "join", "lateral", ","]))
                {
                    continue;
                }
                if let Some((body, after)) = parenthesised(&branch[at..])
                    && is_statement(body)
                    && let Some(alias) = leading_alias(after)
                {
                    let skip = if is(after[0].1, &["as"]) { 2 } else { 1 };
                    let names = after.get(skip..).and_then(parenthesised).map(|(names, _)| names);
                    let fields = self.query(&lowered(body), &local, depth + 1);
                    local.insert(alias, renamed(fields, names).map(Rc::new));
                }
            }
            let aggregates = aggregates(self.engine);
            let sources = sources_of(&branch, &aggregates).map(|(_, sources)| sources);
            let lookup = |tokens: &[Leveled]| {
                self.field(tokens, sources.as_deref(), &local).or_else(|| {
                    let name = unquoted(column(tokens)?.last()?);
                    sources.is_none().then(|| Field {
                        name,
                        nullable: true,
                        ..Field::default()
                    })
                })
            };
            let list = &branch[range];
            let grouped = grouped(&branch);
            let bare = !grouped
                && (rolled(&branch)
                    || (self.engine != Engine::PostgreSql && aggregates_rows(&branch, list, &aggregates)));
            let split = list.split(|&(level, token)| level == 0 && token == ",");
            let mut fields = Vec::new();
            for tokens in split {
                let (expression, alias) = aliased(tokens);
                if let [(_, "*")] | [(_, _), (_, "."), (_, "*")] = expression {
                    let qualifier = (expression.len() == 3).then(|| unquoted(expression[0].1));
                    for source in sources.as_deref()? {
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
                let required = |column: &[Leveled]| lookup(column).map(|f| !f.nullable);
                let facts = item(expression, grouped, bare, &aggregates, required);
                field.nullable |= facts.nullable;
                field.required_elements |= facts.required_elements;
                if self.engine == Engine::PostgreSql
                    && let Some(rows) = array_rows(expression)
                    && let Some(fields) = self.query(&lowered(rows), &local, depth + 1)
                    && matches!(&fields[..], [f] if f.ty.is_some() && !f.nullable && f.problem.is_none())
                {
                    field.required_elements = true;
                }
                if depth > 0 && field.ty.is_none() {
                    field.problem = field.problem.or(Some(DERIVED));
                }
                fields.push(field);
            }
            match &mut output {
                None => output = Some(fields),
                Some(first) if first.len() == fields.len() => {
                    for (left, right) in first.iter_mut().zip(fields) {
                        if left.ty.is_none() || left.ty != right.ty {
                            left.problem = Some(UNION);
                            left.ty = None;
                        }
                        left.nullable |= right.nullable;
                        left.required_elements &= right.required_elements;
                        left.problem = left.problem.or(right.problem);
                    }
                }
                Some(_) => return None,
            }
        }
        output
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
        let (_, sources) = sources_of(rhs, &aggregates(self.engine))?;
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
            nullable: !column.not_null,
            ..Field::default()
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
            field.nullable |= source.outer;
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
                Some(fields) => fields
                    .as_ref()
                    .and_then(|fs| fs.iter().find(|f| f.name == name))
                    .cloned()
                    .or_else(|| {
                        Some(Field {
                            name: name.clone(),
                            nullable: true,
                            problem: Some(DERIVED),
                            ..Field::default()
                        })
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
                field.nullable |= source.outer;
                found = Some(field);
            }
        }
        found
    }

    fn expression(&self, tokens: &[Leveled], lookup: &impl Fn(&[Leveled]) -> Option<Field>, depth: usize) -> Field {
        if depth >= 48 || tokens.is_empty() {
            return Field {
                nullable: true,
                ..Field::default()
            };
        }
        let recurse = |tokens: &[Leveled]| self.expression(tokens, lookup, depth + 1);
        if let Some((body, [])) = parenthesised(tokens) {
            return recurse(body);
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
            return Field {
                nullable: true,
                ..Field::default()
            };
        }
        if let Some(at) = tokens
            .iter()
            .position(|&(d, t)| d == level && is(t, &["and", "or", "like", "ilike", "in", "between"]))
        {
            let nullable = is(tokens[at].1, &["in", "between"])
                || recurse(&tokens[..at]).nullable
                || recurse(&tokens[at + 1..]).nullable;
            return Field {
                ty: Some("bool".into()),
                nullable,
                ..Field::default()
            };
        }
        if is(tokens[0].1, &["not"]) {
            return Field {
                nullable: recurse(&tokens[1..]).nullable,
                ty: Some("bool".into()),
                ..Field::default()
            };
        }
        if let Some(at) = tokens.iter().position(|&(d, t)| d == level && is(t, &["is"])) {
            let tested = &tokens[at + 1..];
            if matches!(tested, [(_, t)] | [(_, "not" | "NOT"), (_, t)] if is(t,&["null","true","false","unknown"])) {
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
                    "+" | "-" | "*" | "/" | "%" | "||" | "=" | "<>" | "!=" | "<" | ">" | "<=" | ">="
                )
        }) && at > 0
        {
            let left = recurse(&tokens[..at]);
            let right = recurse(&tokens[at + 1..]);
            let op = tokens[at].1;
            let nullable =
                left.nullable || right.nullable || (self.engine != Engine::PostgreSql && matches!(op, "/" | "%"));
            return Field {
                nullable,
                ty: matches!(op, "=" | "<>" | "!=" | "<" | ">" | "<=" | ">=").then(|| "bool".into()),
                ..Field::default()
            };
        }
        if let Some((body, ty)) = cast(tokens) {
            let value = recurse(body);
            return Field {
                ty: Some(self.engine.type_name(&ty)),
                problem: None,
                ..value
            };
        }
        if let Some(field) = lookup(tokens) {
            return field;
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
                nullable: (depth > 0 && ty.is_none())
                    || is(value, &["null"])
                    || value.starts_with('$')
                    || *value == "?",
                ty,
                name: unquoted(value),
                ..Field::default()
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
                    .all(|arg| recurse(arg).nullable);
                return Field {
                    name: unquoted(name),
                    nullable,
                    ..Field::default()
                };
            }
        }
        // The existing aggregate and CASE rules know which of their rows can supply NULL.
        let recognized = call(tokens).is_some_and(|(name, _)| is(&name, aggregates(self.engine).nullable))
            || (is(tokens[0].1, &["case"]) && case_literals(tokens).is_some())
            || is(tokens[0].1, &["exists", "array"]);
        Field {
            nullable: !recognized && (depth > 0 || column(tokens).is_none()),
            ..Field::default()
        }
    }
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
