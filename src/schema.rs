//! The catalog sqlc resolved, as the classes we will generate.
//!
//! This is the middle of the pipeline: naming, singularisation and type
//! resolution all happen here, exactly once, so [`crate::emit`] only has to
//! decide how things look.

use std::sync::LazyLock;

use anyhow::Result;
use heck::{ToShoutySnakeCase, ToUpperCamelCase};
use pluralizer::UncountableRule;
use ruff_python_ast as ast;

use crate::config::Config;
use crate::engine::Engine;
use crate::plugin;
use crate::python::{self, Import, Scope, Type};

#[cfg(test)]
#[path = "schema_spec.rs"]
mod spec;

/// Everything a codegen entry will produce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schema {
    pub enums: Vec<Enum>,
    pub models: Vec<Model>,
}

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
    pub ty: FieldType,
}

/// A column's Python type.
///
/// The element type is kept apart from the generics wrapping it, because
/// nullability and array depth are properties of the *column* while the element
/// type is a property of the engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldType {
    pub element: Type,
    /// Nesting depth of a SQL array; zero for a scalar column.
    pub array_dims: usize,
    /// Whether the column admits NULL.
    pub optional: bool,
}

impl Schema {
    /// Reads the catalog sqlc resolved and names everything it will generate.
    pub fn resolve(request: &plugin::GenerateRequest, config: &Config) -> Result<Self> {
        let settings = request.settings.clone().unwrap_or_default();
        let catalog = request.catalog.clone().unwrap_or_default();
        let resolver = &Resolver {
            engine: Engine::parse(&settings.engine)?,
            catalog: &catalog,
        };

        let mut enums: Vec<Enum> = (catalog.user_schemas())
            .flat_map(|schema| {
                schema.enums.iter().map(|sql_enum| Enum {
                    name: resolver.class_name(schema, &sql_enum.name),
                    comment: sql_enum.comment.clone(),
                    members: (sql_enum.vals.iter())
                        .map(|value| Member {
                            name: value.to_shouty_snake_case(),
                            value: value.clone(),
                        })
                        .collect(),
                })
            })
            .collect();

        let mut models: Vec<Model> = (catalog.user_schemas())
            .flat_map(|schema| {
                schema.tables.iter().map(|table| {
                    let relation = table.rel.clone().unwrap_or_default();
                    Model {
                        name: resolver.model_name(schema, &relation.name, config),
                        table: plugin::Identifier {
                            schema: schema.name.clone(),
                            ..relation
                        },
                        comment: table.comment.clone(),
                        fields: (table.columns.iter())
                            .map(|column| Field {
                                name: column.name.clone(),
                                ty: resolver.field_type(column),
                            })
                            .collect(),
                    }
                })
            })
            .collect();

        // Sorting by name keeps output stable across catalog orderings, so a
        // regenerated file only differs when the schema actually changed.
        enums.sort_by(|a, b| a.name.cmp(&b.name));
        models.sort_by(|a, b| a.name.cmp(&b.name));

        Ok(Self { enums, models })
    }
}

impl FieldType {
    /// The generics wrapping the element type, innermost first.
    ///
    /// `Optional` goes outermost because a nullable array column is a list that
    /// may be absent, not a list of independently absent elements.
    ///
    /// This is the one description of what an annotation is made of; both the
    /// expression and the imports it needs are read off it.
    fn wrappers(&self) -> impl Iterator<Item = Type> {
        std::iter::repeat_n(Type::LIST, self.array_dims)
            .chain(self.optional.then_some(Type::OPTIONAL))
    }

    /// The annotation as written from within `scope`, e.g. `Optional[List[int]]`.
    pub fn annotation(&self, scope: Scope<'_>) -> ast::Expr {
        self.wrappers()
            .fold(self.element.expr(scope), |inner, wrapper| {
                python::subscript(wrapper.expr(scope), inner)
            })
    }

    /// The imports the annotation needs.
    pub fn imports(&self) -> impl Iterator<Item = Import> {
        (self.element.import().into_iter()).chain(self.wrappers().filter_map(|w| w.import()))
    }
}

impl Model {
    /// Whether this model describes `table`.
    pub fn is(&self, table: &plugin::Identifier, default_schema: &str) -> bool {
        table.is(&self.table, default_schema)
    }
}

/// Reads types and names out of one catalog.
struct Resolver<'a> {
    engine: Engine,
    catalog: &'a plugin::Catalog,
}

impl Resolver<'_> {
    /// The class name for something declared in `schema`.
    fn class_name(&self, schema: &plugin::Schema, name: &str) -> String {
        schema
            .qualify(name, &self.catalog.default_schema)
            .to_upper_camel_case()
    }

    /// The class name for a table, singularised unless the user opted out.
    fn model_name(&self, schema: &plugin::Schema, table: &str, config: &Config) -> String {
        match config.models.is_inflection_excluded(table) {
            true => self.class_name(schema, table),
            false => self.class_name(schema, &singular(table)),
        }
    }

    fn field_type(&self, column: &plugin::Column) -> FieldType {
        let data_type = column.data_type();
        FieldType {
            element: (self.engine.scalar_type(&data_type))
                .or_else(|| self.declared_type(&data_type))
                .unwrap_or(Type::ANY),
            // sqlc reports both a flag and a depth, and older catalogs only set
            // the flag, so one dimension is the floor for any array column.
            array_dims: usize::from(column.is_array) * (column.array_dims.max(1) as usize),
            optional: !column.not_null,
        }
    }

    /// Looks for a type the schema itself declares: an enum or composite type.
    fn declared_type(&self, data_type: &str) -> Option<Type> {
        // A column names its type either bare or schema-qualified, so match on
        // whichever half identifies it.
        let bare = data_type.rsplit('.').next().unwrap_or(data_type);

        self.catalog.user_schemas().find_map(|schema| {
            let declares = schema.enums.iter().any(|e| e.name == bare)
                || schema.composite_types.iter().any(|c| c.name == bare);
            declares.then(|| Type::Generated(self.class_name(schema, bare)))
        })
    }
}

/// Singularises a table name, so an `authors` table yields an `Author` model.
fn singular(name: &str) -> String {
    // `pluralizer` follows pluralize.js, which treats `species` as a plural of
    // `specie`. Correct it through the crate's own extension point, once.
    static PATCHED: LazyLock<()> = LazyLock::new(|| {
        pluralizer::add_uncountable_rule(UncountableRule::String("species".into()));
    });
    LazyLock::force(&PATCHED);

    pluralizer::pluralize(name, 1, false)
}
