//! The Python names of SQL names that share one scope: a table's columns, a
//! result's, a query's parameters, an enum's labels (`docs/python-output.md`
//! §3).

use std::collections::HashMap;

use anyhow::{Result, bail};
use heck::{ToShoutySnakeCase, ToSnakeCase};
use ruff_python_stdlib::keyword::is_keyword;

use super::models::Member;
use crate::python;

#[cfg(test)]
#[path = "names_spec.rs"]
mod spec;

/// The builtins and locals generated code reads, which a field or parameter
/// must not hide (`docs/python-output.md` §3.1). In a class body, `str: str`
/// makes every later `str` annotation mean the field to mypy and pyright, and
/// `classmethod: str` the `from_row` decorator; in a function, a parameter
/// `row` would hide the row being read. What the code imports is
/// [`python::is_imported`]'s to say.
const RESERVED: &[&str] = &[
    "bool",
    "bytes",
    "classmethod",
    "conn",
    "cur",
    "dict",
    "float",
    "from_row",
    "int",
    "len",
    "list",
    "object",
    "row",
    "rows",
    "str",
    "tuple",
    "value",
];

/// Whether a list of names may repeat one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Repeats {
    /// A table's columns: two columns that spell one field are an error.
    Forbidden,
    /// A result's columns or a query's parameters, where sqlc's Go codegen
    /// numbers repeats (`columnsToStruct`): `issued_on`, `issued_on_2`, ...
    Numbered,
}

/// The Python names of a list of columns or parameters, in order
/// (`docs/python-output.md` §3): each one's snake_case spelling, or
/// `column_<n>` for one sqlc left unnamed, as sqlc's Go codegen does. A
/// keyword, or a name the generated code itself reads ([`RESERVED`], or an
/// import), takes PEP 8's trailing underscore (`from_`, `str_`). Anything else
/// that is not a public Python name is an error, never a guess, and so are two
/// that end up the same.
pub fn names(spellings: &[&str], repeats: Repeats) -> Result<Vec<String>> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    let names = (spellings.iter().enumerate())
        .map(|(i, spelling)| {
            let name = match spelling.is_empty() {
                true => format!("column_{}", i + 1),
                false => spelling.to_snake_case(),
            };
            let count = seen.entry(name.clone()).or_default();
            *count += 1;
            let name = match (repeats, *count) {
                (Repeats::Numbered, repeat @ 2..) => format!("{name}_{repeat}"),
                _ => name,
            };
            let reserved = RESERVED.contains(&name.as_str()) || python::is_imported(&name);
            let name = match is_keyword(&name) || reserved {
                true => name + "_",
                false => name,
            };
            usable(spelling, name)
        })
        .collect::<Result<Vec<_>>>()?;
    distinct(spellings, names)
}

/// An enum's members, in order: each label's words in UPPER_SNAKE_CASE,
/// split as snake_case splits them (`in shock`, `in-shock` and `inShock` are
/// `IN_SHOCK`), with anything not an ASCII letter, digit or `_` dropped.
///
/// A name that is not public is an error: one that is empty or starts with a
/// digit is not an identifier, and one starting with `_` is reserved
/// (`_sunder_`), mangled (`__x`) or private to pyright and ruff (`_1ST`). So
/// are two labels that make one name: Go numbers them `value_<i>`, which
/// would be a member nobody could guess.
pub fn members(labels: &[String]) -> Result<Vec<Member>> {
    let names = (labels.iter())
        .map(|label| {
            let name = (label.to_shouty_snake_case().chars())
                .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            usable(label, name)
        })
        .collect::<Result<_>>()?;
    let names = distinct(labels, names)?;
    Ok((names.into_iter().zip(labels))
        .map(|(name, value)| Member {
            name,
            value: value.clone(),
        })
        .collect())
}

/// `name`, made from `spelling`, if it is a public Python name.
fn usable(spelling: &str, name: String) -> Result<String> {
    match python::is_public_name(&name) {
        true => Ok(name),
        false => bail!("\"{spelling}\" does not become a usable Python name; rename it"),
    }
}

/// `names`, made from `spellings` in order, unless two are one name: in one
/// scope, the second would silently replace the first.
fn distinct(spellings: &[impl AsRef<str>], names: Vec<String>) -> Result<Vec<String>> {
    let mut seen: HashMap<&str, &str> = HashMap::new();
    for (spelling, name) in spellings.iter().zip(&names) {
        if let Some(first) = seen.insert(name, spelling.as_ref()) {
            bail!(
                "\"{first}\" and \"{}\" both become the Python name \"{name}\"",
                spelling.as_ref()
            );
        }
    }
    Ok(names)
}
