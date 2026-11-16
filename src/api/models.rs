use anyhow::{Result, anyhow, bail};
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Enum {
    pub name: String,
    pub comment: String,
    pub members: Vec<Member>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    /// The Python identifier, e.g. `NOT_STARTED`.
    pub name: String,
    /// The value as the database stores it, e.g. `not-started`.
    pub value: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Model {
    /// We match `sqlc.embed` by this table, and reuse the model as a query's row type by it.
    pub table: plugin::Identifier,
    pub name: String,
    pub comment: String,
    pub fields: Vec<Field>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub name: String,
    pub comment: String,
    pub ty: FieldType,
}

/// A column's Python type. We keep the element type apart from the generics around it, as nullability and array depth
/// belong to the column while the element type belongs to the column's database type.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldType {
    pub element: Type,
    pub array_dims: usize,
    pub optional: bool,
    /// The backend returns this array of enums as its text, which the enum's class then parses (`from_array`).
    pub as_text: bool,
}

/// The classes of the models module. A table with a problem still becomes a model, without the fields that have one, so
/// we still check the queries against it.
pub fn resolve(resolver: &Resolver, config: &ModelConfig, problems: &mut Problems) -> (Vec<Enum>, Vec<Model>) {
    let mut enums = Vec::new();
    let mut models = Vec::new();
    for schema in resolver.catalog.user_schemas() {
        for sql_enum in &schema.enums {
            let what = format!("enum \"{}\"", sql_enum.name);
            let name = resolver.class_name(schema, &sql_enum.name);
            problems.take(&what, usable(&name));
            enums.push(Enum {
                name,
                comment: sql_enum.comment.clone(),
                members: problems
                    .take(&what, members(&sql_enum.vals, |label| resolver.spelling(label)))
                    .unwrap_or_default(),
            });
        }
        for table in &schema.tables {
            let relation = table.rel.clone().unwrap_or_default();
            let what = format!("table \"{}\"", relation.name);
            let name = resolver.model_name(schema, &relation.name, config);
            problems.take(&what, usable(&name));
            let fields = resolver.fields(&table.columns, &what, problems);
            models.push(Model {
                name,
                comment: table.comment.clone(),
                fields,
                table: plugin::Identifier {
                    schema: schema.name.clone(),
                    ..relation
                },
            });
        }
    }

    // We sort by name so the output does not depend on the catalog's order, and a regenerated file only differs when
    // the schema did.
    enums.sort_by(|a, b| a.name.cmp(&b.name));
    models.sort_by(|a, b| a.name.cmp(&b.name));

    // All classes share the module's one namespace. Two that would take one name are an error, as in sqlc's Go codegen,
    // because renaming either would be a guess about which one the user means.
    let mut classes: Vec<&str> = (enums.iter().map(|e| e.name.as_str()))
        .chain(models.iter().map(|m| m.name.as_str()))
        .collect();
    classes.sort_unstable();

    // A class called `Decimal` would replace the import every `Decimal`
    // annotation means.
    for name in classes.iter().filter(|name| python::is_imported(name)) {
        problems.note(anyhow!(
            "`{name}` is a name generated code imports; name the table or type otherwise with \
             the rename option"
        ));
    }
    for pair in classes.windows(2).filter(|pair| pair[0] == pair[1]) {
        problems.note(anyhow!(
            "more than one table or type would be the Python class `{}`; name one otherwise with \
             the rename option",
            pair[0]
        ));
    }
    (enums, models)
}

/// Fails for a class name Python cannot take, as the singular of a table `s`, for one, is no name at all.
fn usable(name: &str) -> Result<()> {
    if !python::is_class_name(name) {
        bail!(
            "`{name}` is not a usable Python class name, one in CapWords that hides no builtin; \
             name it with the rename option"
        )
    }
    Ok(())
}

impl FieldType {
    /// The annotation as written in `scope`, such as `list[int] | None`. `None` goes outermost because a nullable array
    /// column is a list that may be absent, not a list of independently absent elements.
    pub fn annotation(&self, scope: Scope) -> ast::Expr {
        self.shaped(self.element.annotation(scope))
    }

    /// The annotation of a parameter of this type ([`Type::parameter_annotation`]).
    pub fn parameter_annotation(&self) -> ast::Expr {
        self.shaped(self.element.parameter_annotation())
    }

    fn shaped(&self, element: ast::Expr) -> ast::Expr {
        let list = (0..self.array_dims).fold(element, |inner, _| LIST.index(inner));
        match self.optional {
            true => list.or_none(),
            false => list,
        }
    }

    /// The annotation as text, for an error to show.
    pub fn describe(&self, scope: Scope) -> String {
        python::unparse(&self.annotation(scope))
    }

    /// Whether [`Self::read`] converts a value.
    pub fn converts(&self) -> bool {
        matches!(self.element, Type::Enum(_) | Type::Converted(_))
    }

    /// The expression that turns `value`, as a driver returns it, into this type.
    /// Drivers return an enum's labels as `str`, so those become members, and a [`Type::Converted`] value becomes its
    /// type; anything else is the value itself.
    pub fn read(&self, value: impl Into<ast::Expr>, scope: Scope) -> ast::Expr {
        let value = value.into();
        if !self.converts() {
            return value;
        }
        let class = self.element.annotation(scope);
        let read = match (self.array_dims, self.as_text) {
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
