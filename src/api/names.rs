use std::collections::HashMap;

use anyhow::{Result, bail};
use heck::{ToShoutySnakeCase, ToSnakeCase};
use ruff_python_stdlib::keyword::is_keyword;

use super::models::Member;
use crate::config::Records;
use crate::python;
use crate::python::vocabulary::{
    BOOL, BYTES, CLASSMETHOD, CONNECTION, CUR, DICT, FLOAT, FROM_ROW, INT, LEN, LIST, OBJECT, ROW, STR, Symbol, TUPLE,
    VALUE,
};

#[cfg(test)]
#[path = "names_spec.rs"]
mod spec;

/// What a class body reads, which a field must not hide: in one, `str: str` makes every later `str` annotation mean the
/// field to mypy and pyright, and `classmethod: str` the decorator of `from_row`. What the code imports is
/// [`python::is_imported`]'s to say.
const CLASS_BODY: &[Symbol] = &[
    BOOL,
    BYTES,
    CLASSMETHOD,
    DICT,
    FLOAT,
    FROM_ROW,
    INT,
    LIST,
    OBJECT,
    STR,
    TUPLE,
];

/// The locals a function reads besides, which a parameter must not hide either: a parameter `row` would hide the row we
/// read. A field cannot, as a class body's names are not a method's.
const FUNCTION_BODY: &[Symbol] = &[CONNECTION, CUR, LEN, ROW, VALUE];

/// What a field of a `pydantic.BaseModel` must not hide: pydantic warns that it shadows the attribute, and a field named
/// `model_config` would replace the configuration. A name starting with one of pydantic's protected prefixes
/// ([`PYDANTIC_PREFIXES`]) warns too.
const PYDANTIC_MODEL: &[&str] = &[
    "construct",
    "copy",
    "dict",
    "from_orm",
    "json",
    "model_computed_fields",
    "model_config",
    "model_construct",
    "model_copy",
    "model_extra",
    "model_fields",
    "model_fields_set",
    "model_json_schema",
    "model_parametrized_name",
    "model_post_init",
    "model_rebuild",
    "parse_file",
    "parse_obj",
    "parse_raw",
    "schema",
    "schema_json",
    "update_forward_refs",
    "validate",
];

const PYDANTIC_PREFIXES: &[&str] = &["model_dump", "model_validate"];

/// Where a name is bound, which decides what it must not hide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Binding {
    Field,
    Parameter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Repeats {
    /// A table's columns, where two columns that spell one field are an error.
    Forbidden,
    /// A result's columns or a query's parameters, where we number repeats as sqlc's Go codegen does
    /// (`columnsToStruct`): `issued_on`, `issued_on_2`, ...
    Numbered,
}

/// The Python names of a list of columns or parameters, in order. Each is its snake_case
/// spelling, or `column_<n>` for one sqlc left unnamed as sqlc's Go codegen does, and a keyword or a name the generated
/// code reads where it is bound ([`CLASS_BODY`], [`FUNCTION_BODY`], an import, or a pydantic model's own attribute in
/// one, [`PYDANTIC_MODEL`]) takes PEP 8's trailing underscore (`from_`, `str_`). Anything else
/// that is not a public Python name is an error and never a guess, and so are two names that end up the same.
pub fn names(spellings: &[&str], repeats: Repeats, binding: Binding, records: Records) -> Result<Vec<String>> {
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
            let reads = |body: &[Symbol]| body.iter().any(|reserved| reserved.as_ref() == name);
            let is_pydantic = records == Records::Pydantic
                && (PYDANTIC_MODEL.contains(&name.as_str()) || PYDANTIC_PREFIXES.iter().any(|p| name.starts_with(p)));
            let reserved = reads(CLASS_BODY)
                || (binding == Binding::Parameter && reads(FUNCTION_BODY))
                || is_pydantic
                || python::is_imported(&name);
            let name = match is_keyword(&name) || reserved {
                true => name + "_",
                false => name,
            };
            usable(spelling, name)
        })
        .collect::<Result<Vec<_>>>()?;
    distinct(spellings, names)
}

/// An enum's members, in order: the words of each label, as `spelling` spells it, in UPPER_SNAKE_CASE and split as
/// snake_case splits them (`in shock`, `in-shock` and `inShock` are all `IN_SHOCK`), dropping anything that is not an
/// ASCII letter, digit or `_`.
///
/// A name that is not public is an error: an empty one or one starting with a digit is no identifier, and one starting
/// with `_` is reserved (`_sunder_`), mangled (`__x`) or private to pyright and ruff (`_1ST`). Two labels that make one
/// name are an error too, as Go's `value_<i>` numbering gives a member nobody could guess.
pub fn members<'l>(labels: &'l [String], spelling: impl Fn(&'l str) -> &'l str) -> Result<Vec<Member>> {
    let names = (labels.iter())
        .map(|label| {
            let name = (spelling(label).to_shouty_snake_case().chars())
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

fn usable(spelling: &str, name: String) -> Result<String> {
    match python::is_public_name(&name) {
        true => Ok(name),
        false => bail!(
            "\"{spelling}\" does not become a usable Python name; give it one with the rename \
             option"
        ),
    }
}

/// Fails when two of the names are one, as in one scope the second would silently replace the first.
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
