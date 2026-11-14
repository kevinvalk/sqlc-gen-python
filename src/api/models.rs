//! The catalog sqlc resolved, as the classes we will generate.
//!
//! Naming, singularisation and type resolution all happen here, exactly once
//! ([`super::resolver`], [`super::names`]), so [`crate::render`] only has to
//! lay the classes out. [`super::queries`] resolves the queries the same
//! way, against these classes.

use anyhow::anyhow;
use ruff_python_ast as ast;

use super::Problems;
use super::names::members;
use super::resolver::Resolver;
use crate::config::ModelConfig;
use crate::plugin;
use crate::python::vocabulary::{FROM_ARRAY, LIST, V};
use crate::python::{self, Expression, Scope, Type, each, list_of, unless_none};

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
#[derive(Debug, Clone, PartialEq)]
pub struct Model {
    /// The table this models, used to match `sqlc.embed` and to reuse a model
    /// as a query's return type.
    pub table: plugin::Identifier,
    pub name: String,
    pub comment: String,
    pub fields: Vec<Field>,
}

/// One column, as a class attribute.
#[derive(Debug, Clone, PartialEq)]
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
#[derive(Debug, Clone, PartialEq)]
pub struct FieldType {
    pub element: Type,
    /// Nesting depth of a SQL array; zero for a scalar column.
    pub array_dims: usize,
    /// Whether the column admits NULL.
    pub optional: bool,
}

/// The enums and models of the catalog `resolver` reads: the classes of the
/// models module. A table with a problem is a model without the fields that
/// have one, so that the queries of it are still checked.
pub fn resolve(
    resolver: &Resolver,
    config: &ModelConfig,
    problems: &mut Problems,
) -> (Vec<Enum>, Vec<Model>) {
    let mut enums = Vec::new();
    let mut models = Vec::new();
    for schema in resolver.catalog.user_schemas() {
        for sql_enum in &schema.enums {
            let what = format!("enum \"{}\"", sql_enum.name);
            enums.push(Enum {
                name: resolver.class_name(schema, &sql_enum.name),
                comment: sql_enum.comment.clone(),
                members: problems
                    .take(&what, members(&sql_enum.vals))
                    .unwrap_or_default(),
            });
        }
        for table in &schema.tables {
            let relation = table.rel.clone().unwrap_or_default();
            let what = format!("table \"{}\"", relation.name);
            let fields = resolver.fields(&table.columns, &what, problems);
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
    for name in classes.iter().filter(|name| !python::is_class_name(name)) {
        problems.note(anyhow!(
            "`{name}` is not a usable Python class name, one in CapWords that hides no builtin; \
             rename the table or type"
        ));
    }
    // A class called `Decimal` would replace the import every `Decimal`
    // annotation means.
    for name in classes.iter().filter(|name| python::is_imported(name)) {
        problems.note(anyhow!(
            "`{name}` is a name generated code imports; rename the table or type"
        ));
    }
    for pair in classes.windows(2).filter(|pair| pair[0] == pair[1]) {
        problems.note(anyhow!(
            "more than one table or type would be the Python class `{}`; rename one",
            pair[0]
        ));
    }
    (enums, models)
}

impl FieldType {
    /// The annotation as written from within `scope`, e.g. `list[int] | None`.
    ///
    /// `None` goes outermost because a nullable array column is a list that may
    /// be absent, not a list of independently absent elements.
    pub fn annotation(&self, scope: Scope) -> ast::Expr {
        let element = self.element.annotation(scope);
        let list = (0..self.array_dims).fold(element, |inner, _| LIST.index(inner));
        match self.optional {
            true => list.or_none(),
            false => list,
        }
    }

    /// The annotation, as written, for an error to show.
    pub fn describe(&self, scope: Scope) -> String {
        python::unparse(&self.annotation(scope))
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
    pub fn read(&self, value: impl Into<ast::Expr>, scope: Scope, text_arrays: bool) -> ast::Expr {
        let value = value.into();
        if !self.converts() {
            return value;
        }
        let class = self.element.annotation(scope);
        let read = match (self.array_dims, text_arrays) {
            (0, _) => class.call([value.clone()]),
            (_, false) => list_of(class.call([V.into()]), each(V, value.clone())),
            (_, true) => class.attr(FROM_ARRAY).call([value.clone()]),
        };
        match self.optional {
            true => unless_none(value, read),
            false => read,
        }
    }
}
