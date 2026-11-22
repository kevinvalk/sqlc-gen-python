use std::collections::BTreeSet;

use anyhow::{Result, anyhow, bail};
use ruff_python_ast as ast;

use super::Problems;
use super::names::members;
use super::resolver::Resolver;
use crate::plugin;
use crate::python::vocabulary::{FROM_ARRAY, LIST, MULTIPLE_ROWS_ERROR, NO_ROW_ERROR, Symbol, TYPING, V};
use crate::python::{self, Expression, Scope, Type, each, forward, list_of, name, unless_none};

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
    /// PostgreSQL allows NULL leaves unless a SELECT expression proves every element present.
    pub collects_nulls: bool,
}

/// The classes of the models module, or those of them `used` names. A table with a problem still becomes a model,
/// without the fields that have one, so we still check the queries against it.
pub fn resolve(
    resolver: &Resolver,
    used: Option<&BTreeSet<String>>,
    problems: &mut Problems,
) -> (Vec<Enum>, Vec<Model>) {
    let is_used = |name: &String| used.is_none_or(|used| used.contains(name));
    let mut enums = Vec::new();
    let mut models = Vec::new();
    for schema in resolver.catalog.user_schemas() {
        for sql_enum in schema
            .enums
            .iter()
            .filter(|labels| !crate::sqlc_typing::is_set(resolver.engine, schema, labels))
        {
            let what = format!("enum \"{}\"", sql_enum.name);
            let name = resolver.class_name(schema, &sql_enum.name);
            if !is_used(&name) {
                continue;
            }
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
            let name = resolver.model_name(schema, &relation.name);
            if !is_used(&name) {
                continue;
            }
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
    // A driver has no errors for a `:one`, so there the module defines them, and a class of either name would replace
    // the error: `raise models.NoRowError` then raises a `TypeError`.
    let errors = match resolver.config.backend.defines_errors() {
        true => [NO_ROW_ERROR, MULTIPLE_ROWS_ERROR].as_slice(),
        false => &[],
    };
    for name in classes
        .iter()
        .filter(|name| errors.iter().any(|error| error.as_ref() == **name))
    {
        problems.note(anyhow!(
            "`{name}` is a class the models define; name the table or type otherwise with the rename option"
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
    /// The annotation as written in `scope`. Column nullability belongs to the whole value, even for an array.
    pub fn annotation(&self, scope: Scope) -> ast::Expr {
        self.shaped(self.element.annotation(scope), false)
    }

    /// The annotation of a parameter of this type ([`Type::parameter_annotation`]).
    pub fn parameter_annotation(&self) -> ast::Expr {
        self.shaped(self.element.parameter_annotation(), true)
    }

    fn shaped(&self, element: ast::Expr, parameter: bool) -> ast::Expr {
        let array = |leaf| (0..self.array_dims).fold(leaf, |inner, _| LIST.index(inner));
        let ty = if self.nullable_elements() {
            let nullable = array(element.clone().or_none());
            // A parameter only reads its lists. Python's invariant list type would otherwise refuse a caller's
            // list[UUID] for a uuid[] that also admits NULL elements.
            if parameter {
                nullable.union(array(element))
            } else {
                nullable
            }
        } else {
            array(element)
        };
        match self.optional {
            true => ty.or_none(),
            false => ty,
        }
    }

    /// The annotation as text, for an error to show.
    pub fn describe(&self, scope: Scope) -> String {
        python::unparse(&self.annotation(scope))
    }

    fn nullable_elements(&self) -> bool {
        self.array_dims > 0 && (self.collects_nulls || self.element.converters().is_some())
    }

    /// The scalar or each array element, preserving SQL NULL around the column and at a converted leaf.
    fn map_elements(&self, value: ast::Expr, convert: impl FnOnce(ast::Expr) -> ast::Expr) -> ast::Expr {
        let item = |depth: usize| format!("_{}{depth}", V.as_ref());
        let at = |depth| if depth == 0 { value.clone() } else { name(item(depth)) };
        let leaf = at(self.array_dims);
        let mut mapped = convert(leaf.clone());
        if self.nullable_elements() {
            mapped = unless_none(leaf, mapped);
        }
        for depth in (1..=self.array_dims).rev() {
            mapped = list_of(mapped, each(item(depth), at(depth - 1)));
        }
        if self.optional {
            unless_none(value, mapped)
        } else {
            mapped
        }
    }

    /// A driver's JSON wrapper takes the document after the user's conversion.
    pub fn bind(&self, value: ast::Expr, wrapper: Option<Symbol>) -> ast::Expr {
        let converters = self.element.converters();
        if converters.is_none() && wrapper.is_none() {
            return value;
        }
        self.map_elements(value, |element| {
            let encoded = match converters {
                Some(pair) => pair.to_db.clone().call([element]),
                None => element,
            };
            match wrapper {
                Some(wrapper) => wrapper.call([encoded]),
                None => encoded,
            }
        })
    }

    /// Whether [`Self::read`] converts a value.
    pub fn converts(&self) -> bool {
        self.array_dims > 0
            || matches!(self.element, Type::Enum(_) | Type::Converted(_))
            || self.element.converters().is_some()
    }

    /// The expression that turns `value`, as a driver returns it, into this type.
    /// Drivers return an enum's labels as `str`, so those become members, and a [`Type::Converted`] value becomes its
    /// type; anything else is the value itself.
    pub fn read(&self, value: impl Into<ast::Expr>, scope: Scope) -> ast::Expr {
        let value = value.into();
        if !self.converts() {
            return value;
        }
        let raw = if self.array_dims > 0 && !self.as_text {
            // A JSON decoder or the application's loader for an unmapped type can return a list for one element.
            let wire = self.element.wire();
            let options = (matches!(wire, Type::Json(_)) || *wire == Type::OBJECT)
                .then(|| python::keyword("list_leaves", python::true_()));
            name("_checked_array").call_with([value.clone(), python::int(self.array_dims)], options)
        } else {
            value.clone()
        };
        let required = Self {
            optional: false,
            ..self.clone()
        };
        let read = if let Some(converters) = self.element.converters() {
            required.map_elements(raw, |element| {
                let raw = match self.element.wire() {
                    Type::Converted(builtin) => builtin.call([element]),
                    wire => TYPING.attr("cast").call([forward(wire.annotation(scope)), element]),
                };
                converters.from_db.clone().call([raw])
            })
        } else if self.as_text {
            let class = self.element.annotation(scope);
            if self.collects_nulls {
                class.attr(FROM_ARRAY).call([raw])
            } else {
                enum_array(raw, class, false)
            }
        } else if matches!(self.element, Type::Enum(_) | Type::Converted(_)) {
            required.map_elements(raw, |leaf| self.element.annotation(scope).call([leaf]))
        } else {
            raw
        };
        if self.optional { unless_none(value, read) } else { read }
    }
}

/// The enum labels we support in PostgreSQL's array text need no quoting. The unquoted word NULL denotes a missing element.
pub(crate) fn enum_array(value: ast::Expr, class: ast::Expr, nullable: bool) -> ast::Expr {
    use crate::python::vocabulary::LABEL;
    let labels = value
        .slice(python::int(1), python::minus(python::int(1)))
        .attr("split")
        .call([python::string(",")]);
    let member = class.call([LABEL.into()]);
    let member = if nullable {
        python::none_if(LABEL.equals(python::string("NULL")), member)
    } else {
        member
    };
    list_of(member, each(LABEL, labels).when(LABEL))
}
