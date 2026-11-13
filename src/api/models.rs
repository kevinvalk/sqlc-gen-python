//! The catalog sqlc resolved, as the classes we will generate.
//!
//! Naming, singularisation and type resolution all happen here, exactly once,
//! so [`crate::render`] only has to decide how things look. [`super::queries`]
//! resolves the queries the same way, against these classes.

use std::collections::HashMap;
use std::sync::LazyLock;

use anyhow::{Context, Result, bail};
use heck::{ToSnakeCase, ToUpperCamelCase};
use ruff_python_stdlib::keyword::is_keyword;

use super::inflection::singular;
use crate::config::ModelConfig;
use crate::engine::Engine;
use crate::plugin;
use crate::python::{self, Scope, Type};

#[cfg(test)]
#[path = "models_spec.rs"]
mod spec;

/// A SQL enum type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Enum {
    pub name: String,
    pub comment: String,
    pub members: Vec<Member>,
}

/// One label of a SQL enum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    /// The Python identifier, e.g. `NOT_STARTED`.
    pub name: String,
    /// The value as the database stores it, e.g. `not-started`.
    pub value: String,
}

/// A table, as a Python class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Model {
    /// The table this models, used to match `sqlc.embed` and to reuse a model
    /// as a query's return type.
    pub table: plugin::Identifier,
    pub name: String,
    pub comment: String,
    pub fields: Vec<Field>,
}

/// One column, as a class attribute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    pub comment: String,
    pub ty: FieldType,
}

/// A column's Python type.
///
/// The element type is kept apart from the generics wrapping it, because
/// nullability and array depth are properties of the *column* while the element
/// type is a property of the column's database type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldType {
    pub element: Type,
    /// Nesting depth of a SQL array; zero for a scalar column.
    pub array_dims: usize,
    /// Whether the column admits NULL.
    pub optional: bool,
}

/// The enums and models of the catalog `resolver` reads: the classes of the
/// models module.
pub fn resolve(resolver: &Resolver, config: &ModelConfig) -> Result<(Vec<Enum>, Vec<Model>)> {
    let mut enums = Vec::new();
    let mut models = Vec::new();
    for schema in resolver.catalog.user_schemas() {
        for sql_enum in &schema.enums {
            enums.push(Enum {
                name: resolver.class_name(schema, &sql_enum.name),
                comment: sql_enum.comment.clone(),
                members: (members(&sql_enum.vals))
                    .with_context(|| format!("enum \"{}\"", sql_enum.name))?,
            });
        }
        for table in &schema.tables {
            let relation = table.rel.clone().unwrap_or_default();
            let fields = (resolver.fields(&table.columns))
                .with_context(|| format!("table \"{}\"", relation.name))?;
            models.push(Model {
                name: resolver.model_name(schema, &relation.name, config),
                comment: table.comment.clone(),
                fields,
                table: plugin::Identifier {
                    schema: schema.name.clone(),
                    ..relation
                },
            });
        }
    }

    // Sorting by name keeps output stable across catalog orderings, so a
    // regenerated file only differs when the schema actually changed.
    enums.sort_by(|a, b| a.name.cmp(&b.name));
    models.sort_by(|a, b| a.name.cmp(&b.name));

    // Classes share the module's one namespace. Two objects that would take
    // one name are an error, as in sqlc's Go codegen: renaming either would
    // be a guess about which one the user means.
    let mut classes: Vec<&str> = (enums.iter().map(|e| e.name.as_str()))
        .chain(models.iter().map(|m| m.name.as_str()))
        .collect();
    classes.sort_unstable();
    if let Some(name) = classes.iter().find(|name| !python::is_public_name(name)) {
        bail!("`{name}` is not a usable Python class name; rename the table or type");
    }
    // A class called `Decimal` would replace the import every `Decimal`
    // annotation means.
    if let Some(name) = classes.iter().find(|name| python::is_imported(name)) {
        bail!("`{name}` is a name generated code imports; rename the table or type");
    }
    if let Some(pair) = classes.windows(2).find(|pair| pair[0] == pair[1]) {
        bail!(
            "more than one table or type would be the Python class `{}`; rename one",
            pair[0]
        );
    }
    Ok((enums, models))
}

impl FieldType {
    /// The annotation as written from within `scope`, e.g. `list[int] | None`.
    ///
    /// `None` goes outermost because a nullable array column is a list that may
    /// be absent, not a list of independently absent elements.
    pub fn annotation(&self, scope: Scope) -> String {
        let list = (0..self.array_dims).fold(self.element.source(scope), |inner, _| {
            format!("list[{inner}]")
        });
        match self.optional {
            true => format!("{list} | None"),
            false => list,
        }
    }

    /// Whether [`Self::read`] converts a value.
    pub fn converts(&self) -> bool {
        matches!(self.element, Type::Enum(_) | Type::Converted(_))
    }

    /// The expression that turns `value`, as a driver returns it, into this
    /// type (`docs/python-output.md` §4, rule 4): the value itself, except
    /// that an enum's labels, which drivers return as `str`, become members,
    /// and a [`Type::Converted`] value its type. A driver that returns an
    /// array of enums as its text (`text_arrays`) has it parsed by the enum.
    pub fn read(&self, value: &str, scope: Scope, text_arrays: bool) -> String {
        if !self.converts() {
            return value.into();
        }
        let class = self.element.source(scope);
        let read = match (self.array_dims, text_arrays) {
            (0, _) => format!("{class}({value})"),
            (_, false) => format!("[{class}(v) for v in {value}]"),
            (_, true) => format!("{class}.from_array({value})"),
        };
        match self.optional {
            true => format!("None if {value} is None else {read}"),
            false => read,
        }
    }
}

/// Reads types and names out of one catalog, by its engine's type map.
pub struct Resolver<'a> {
    pub engine: Engine,
    pub catalog: &'a plugin::Catalog,
}

impl<'a> Resolver<'a> {
    /// The resolver of the catalog in `request`, for its engine.
    pub fn new(request: &'a plugin::GenerateRequest) -> Result<Self> {
        static EMPTY: LazyLock<plugin::Catalog> = LazyLock::new(plugin::Catalog::default);
        Ok(Self {
            engine: Engine::of(request)?,
            catalog: request.catalog.as_ref().unwrap_or(&EMPTY),
        })
    }

    /// The class name for something declared in `schema`. Objects in the
    /// default schema keep their bare name; everything else is qualified, so
    /// `analytics.event` becomes `AnalyticsEvent`.
    fn class_name(&self, schema: &plugin::Schema, name: &str) -> String {
        match schema.name == self.catalog.default_schema {
            true => name.to_upper_camel_case(),
            false => format!("{}_{name}", schema.name).to_upper_camel_case(),
        }
    }

    /// The class name for a table, singularised unless the user opted out.
    fn model_name(&self, schema: &plugin::Schema, table: &str, config: &ModelConfig) -> String {
        match config.is_inflection_excluded(table) {
            true => self.class_name(schema, table),
            false => self.class_name(schema, &singular(table)),
        }
    }

    /// A table's columns, as fields in column order.
    fn fields(&self, columns: &[plugin::Column]) -> Result<Vec<Field>> {
        let spellings: Vec<&str> = columns.iter().map(|c| c.name.as_str()).collect();
        (names(&spellings, Repeats::Forbidden)?
            .into_iter()
            .zip(columns))
        .map(|(name, column)| self.field(name, column))
        .collect()
    }

    /// `column` as the field `name`. A column sqlc left unnamed is described
    /// by that name in errors.
    pub fn field(&self, name: String, column: &plugin::Column) -> Result<Field> {
        let described = match column.name.as_str() {
            "" => &name,
            spelling => spelling,
        };
        // sqlc reports every cast NOT NULL, so one of a nullable value lies.
        let cast = format!(
            "{} if it can be NULL, since sqlc reports every cast NOT NULL",
            self.engine.cast("coalesce(<expr>, <default>)")
        );
        // sqlc reports `coalesce(x, ...)` as the column `x`, but NOT NULL: the
        // type is the first argument's, whatever PostgreSQL resolves
        // (`coalesce(qty, 1.5)` is numeric), so it is no type to trust either.
        if column.not_null && self.source(column).is_some_and(|source| !source.not_null) {
            bail!("column \"{described}\" has a type sqlc could not infer; add a cast, as {cast}")
        }
        Ok(Field {
            ty: self.field_type(column, &format!("column \"{described}\""), &cast)?,
            comment: column.comment.clone(),
            name,
        })
    }

    /// The catalog column a result column is read from, as sqlc reports it.
    fn source(&self, column: &plugin::Column) -> Option<&plugin::Column> {
        let table = column.table.as_ref()?;
        (self.catalog.schema(&table.schema)?.tables.iter())
            .find(|t| t.rel.as_ref().is_some_and(|rel| rel.name == table.name))?
            .columns
            .iter()
            .find(|c| c.name == column.original_name)
    }

    /// The type of `column`, which `what` describes in errors, or an error for
    /// a type without a verified Python type: annotating it `Any` would hide
    /// exactly the mistakes the type checkers are there to catch. For a type
    /// sqlc could not infer, the error suggests `cast`.
    ///
    /// A column sqlc marks `is_func_call` has the return type of an overload
    /// it guessed (`abs(numeric)` as `bigint`), and other guesses are
    /// [`Engine::is_guess`]'s to tell (`docs/python-output.md` §5, rule 2).
    pub fn field_type(&self, column: &plugin::Column, what: &str, cast: &str) -> Result<FieldType> {
        let data_type = column.data_type();
        if column.is_func_call || self.engine.is_guess(&data_type) {
            bail!("{what} has a type sqlc could not infer; add a cast, as {cast}")
        }
        let Some(element) = self
            .engine
            .scalar_type(&data_type)
            .or_else(|| self.declared_type(&data_type))
        else {
            // sqlc sends a domain's name, never its base type.
            let domain =
                match self.engine == Engine::PostgreSql && !data_type.starts_with("pg_catalog.") {
                    false => "",
                    true => {
                        "; for a domain, declare the column with its base type and a CHECK \
                          constraint"
                    }
                };
            bail!("{what} has type \"{data_type}\", which has no verified Python mapping{domain}")
        };
        let array_dims = column.array_dims as usize;
        if column.is_array && array_dims == 0 {
            bail!("{what} is an array, but sqlc reported no array depth for it")
        }
        if array_dims > 0
            && let Some(problem) = self.engine.array_problem(&data_type)
        {
            bail!("{what} is an array of \"{data_type}\", which {problem}")
        }
        if matches!(element, Type::Enum(_)) && array_dims > 1 {
            bail!(
                "{what} is a multi-dimensional array of enum \"{data_type}\", which has no verified Python mapping"
            )
        }
        Ok(FieldType {
            element,
            array_dims,
            optional: !column.not_null,
        })
    }

    /// Looks for an enum the schema itself declares.
    ///
    /// A column names its type `schema.name`, or bare for the default schema
    /// (`docs/python-output.md` §5, rule 3). Composite types get no class yet,
    /// so a column of one is as unknown as a type nothing declares.
    fn declared_type(&self, data_type: &str) -> Option<Type> {
        let (schema, name) = data_type.rsplit_once('.').unwrap_or(("", data_type));
        let schema = self.catalog.schema(schema)?;
        (schema.enums.iter().any(|e| e.name == name))
            .then(|| Type::Enum(self.class_name(schema, name)))
    }
}

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
    "float",
    "from_row",
    "int",
    "list",
    "object",
    "row",
    "rows",
    "str",
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

/// An enum's members, in order: each label as sqlc's Go codegen spells it
/// (`EnumReplace`: `-`, `/`, `:` and `_` become `_`, anything else that is
/// not an ASCII letter or digit is dropped), upper-cased.
///
/// A name that is not public is an error: one that is empty or starts with a
/// digit is not an identifier, and one starting with `_` is reserved
/// (`_sunder_`), mangled (`__x`) or private to pyright and ruff (`_1ST`). So
/// are two labels that make one name: Go numbers them `value_<i>`, which
/// would be a member nobody could guess.
pub fn members(labels: &[String]) -> Result<Vec<Member>> {
    let names = (labels.iter())
        .map(|label| {
            let name = (label.chars())
                .filter_map(|c| match c {
                    '-' | '/' | ':' | '_' => Some('_'),
                    c if c.is_ascii_alphanumeric() => Some(c.to_ascii_uppercase()),
                    _ => None,
                })
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
