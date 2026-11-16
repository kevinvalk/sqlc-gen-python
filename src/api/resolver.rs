use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use anyhow::{Result, bail};
use heck::ToUpperCamelCase;

use super::Problems;
use super::inflection::singular;
use super::models::{Field, FieldType};
use super::names::{Binding, Repeats, names};
use crate::backend::Backend;
use crate::config::{Config, ModelConfig, Override};
use crate::engine::Engine;
use crate::plugin;
use crate::python::Type;

#[cfg(test)]
#[path = "resolver_spec.rs"]
mod spec;

/// Reads the Python type of each column of one catalog, by its engine's type map and the user's overrides, and the
/// Python spelling of each SQL name, by the user's renames.
pub struct Resolver<'a> {
    pub engine: Engine,
    pub catalog: &'a plugin::Catalog,
    overrides: &'a [Override],
    rename: &'a BTreeMap<String, String>,
    backend: Backend,
    /// The rename keys and overrides (by index) that matched something, as any other is a typo that would silently do
    /// nothing.
    used: RefCell<(BTreeSet<&'a str>, BTreeSet<usize>)>,
}

impl<'a> Resolver<'a> {
    pub fn new(request: &'a plugin::GenerateRequest, config: &'a Config) -> Result<Self> {
        static EMPTY: LazyLock<plugin::Catalog> = LazyLock::new(plugin::Catalog::default);
        Ok(Self {
            engine: Engine::of(request)?,
            catalog: request.catalog.as_ref().unwrap_or(&EMPTY),
            overrides: &config.overrides,
            rename: &config.rename,
            backend: config.backend,
            used: RefCell::default(),
        })
    }

    /// Fails for a rename key or an override that matched nothing.
    pub fn check_used(&self) -> Result<()> {
        let (keys, overrides) = &*self.used.borrow();
        if let Some(key) = self.rename.keys().find(|key| !keys.contains(key.as_str())) {
            bail!("rename: \"{key}\" names no table, type, column or parameter of the queries");
        }
        // Entries often share one list of overrides (a YAML anchor), so one of them may well not use an override,
        // which is then no typo as long as it names a type or a column of the schema
        if let Some(i) =
            (0..self.overrides.len()).find(|i| !overrides.contains(i) && !self.in_schema(&self.overrides[*i]))
        {
            bail!(
                "overrides[{i}]: matches no column or parameter that an override before it does \
                 not, nor a type or column of the schema"
            );
        }
        Ok(())
    }

    /// Whether `rule` names a type a column of the schema has, a type it declares, or one of its columns.
    fn in_schema(&self, rule: &Override) -> bool {
        let schemas = || self.catalog.user_schemas();
        let tables = || schemas().flat_map(|schema| schema.tables.iter().map(move |table| (schema, table)));
        let names_type = |ty: &str| {
            let declared = schemas().any(|schema| {
                let named = |name: &str| self.plain(&format!("{}.{name}", schema.name)) == self.plain(ty);
                schema.enums.iter().any(|e| named(&e.name)) || schema.composite_types.iter().any(|c| named(&c.name))
            });
            declared
                || tables()
                    .flat_map(|(_, table)| &table.columns)
                    .any(|c| self.names_type(ty, &c.data_type()))
        };
        let names_column = |named: &str| {
            tables().any(|(schema, table)| {
                let name = table.rel.as_ref().map_or("", |rel| rel.name.as_str());
                table.columns.iter().any(|column| {
                    let qualified = format!("{}.{name}.{}", schema.name, column.name);
                    named == qualified
                        || (schema.name == self.catalog.default_schema && named == format!("{name}.{}", column.name))
                })
            })
        };
        rule.db_type.as_deref().is_some_and(names_type) || rule.column.as_deref().is_some_and(names_column)
    }

    /// Whether an override's `ty` names `data_type`: as written, or bare for PostgreSQL's own, the default schema's, or
    /// an extension's, which no schema declares and may be in any (`vector` is `extensions.vector`). A bare name of
    /// PostgreSQL's own is never another schema's type, which is a domain of the user's (`archive.money`).
    fn names_type(&self, ty: &str, data_type: &str) -> bool {
        let is_extensions = !ty.contains('.') && !self.declares(data_type) && !self.engine.is_own_type(ty);
        let bare = |name: &str| {
            name.rsplit_once('.')
                .map_or(name, |(_, name)| name)
                .to_ascii_lowercase()
        };
        self.plain(ty) == self.plain(data_type) || (is_extensions && bare(data_type) == ty.to_ascii_lowercase())
    }

    /// A type's name as an override may write it: bare when it is PostgreSQL's own or the default schema's.
    fn plain(&self, name: &str) -> String {
        let name = name.to_ascii_lowercase();
        let default = format!("{}.", self.catalog.default_schema);
        (name.strip_prefix("pg_catalog.").or_else(|| name.strip_prefix(&default)))
            .map(String::from)
            .unwrap_or(name)
    }

    /// The rename of `key`, if any, which we then note as used.
    fn rename(&self, key: &str) -> Option<&'a str> {
        let (key, renamed) = self.rename.get_key_value(key)?;
        self.used.borrow_mut().0.insert(key);
        Some(renamed)
    }

    /// How we spell the SQL name `name` in Python: its rename, or itself.
    pub fn spelling<'s>(&'s self, name: &'s str) -> &'s str {
        self.rename(name).unwrap_or(name)
    }

    /// A class is renamed by `schema.name`, or by its bare name in the default schema.
    fn renamed_class(&self, schema: &plugin::Schema, name: &str) -> Option<&'a str> {
        (self.rename(&format!("{}.{name}", schema.name)))
            .or_else(|| (schema.name == self.catalog.default_schema).then(|| self.rename(name))?)
    }

    /// Objects in the default schema keep their bare name and everything else is qualified, so `analytics.event`
    /// becomes `AnalyticsEvent`, unless renamed.
    pub fn class_name(&self, schema: &plugin::Schema, name: &str) -> String {
        match self.renamed_class(schema, name) {
            Some(renamed) => renamed.into(),
            None => self.spelt(schema, name),
        }
    }

    fn spelt(&self, schema: &plugin::Schema, name: &str) -> String {
        match schema.name == self.catalog.default_schema {
            true => name.to_upper_camel_case(),
            false => format!("{}_{name}", schema.name).to_upper_camel_case(),
        }
    }

    /// The override of `column`, of type `data_type`, where one naming the column itself wins over one naming its type.
    fn overridden(&self, column: &plugin::Column, data_type: &str) -> Option<&'a Override> {
        let names_column = |rule: &&Override| {
            let table = column.table.as_ref()?;
            let name = match column.original_name.as_str() {
                "" => &column.name,
                original => original,
            };
            let schema = match table.schema.as_str() {
                "" => &self.catalog.default_schema,
                schema => schema,
            };
            let named = rule.column.as_deref()?;
            // A bare `table.column` is the default schema's.
            ((named == format!("{}.{name}", table.name) && *schema == self.catalog.default_schema)
                || named == format!("{schema}.{}.{name}", table.name))
            .then_some(())
        };

        let i = (self.overrides.iter().position(|rule| names_column(&rule).is_some())).or_else(|| {
            (self.overrides.iter())
                .position(|rule| rule.db_type.as_deref().is_some_and(|ty| self.names_type(ty, data_type)))
        })?;
        self.used.borrow_mut().1.insert(i);
        Some(&self.overrides[i])
    }

    /// The class name of a table, singular unless the user opted out.
    pub fn model_name(&self, schema: &plugin::Schema, table: &str, config: &ModelConfig) -> String {
        // We rename by the table's name alone and never by its singular, as that may well be a column's name
        if let Some(renamed) = self.renamed_class(schema, table) {
            return renamed.into();
        }

        match config.is_inflection_excluded(table) {
            true => self.spelt(schema, table),
            false => self.spelt(schema, &singular(table)),
        }
    }

    /// A table's columns as fields, in order, leaving out each one with a problem after noting it under `what`.
    pub fn fields(&self, columns: &[plugin::Column], what: &str, problems: &mut Problems) -> Vec<Field> {
        let spellings: Vec<&str> = columns.iter().map(|c| self.spelling(&c.name)).collect();
        let names = problems.take(what, names(&spellings, Repeats::Forbidden, Binding::Field));
        (names.unwrap_or_default().into_iter().zip(columns))
            .filter_map(|(name, column)| problems.take(what, self.field(name, column)))
            .collect()
    }

    /// A column sqlc left unnamed is described by `name` in errors.
    pub fn field(&self, name: String, column: &plugin::Column) -> Result<Field> {
        let described = match column.name.as_str() {
            "" => &name,
            spelling => spelling,
        };

        // sqlc reports every cast NOT NULL, so a cast of a nullable value lies
        let cast = format!(
            "{}, or {} if it can be NULL, as sqlc reports every cast NOT NULL",
            self.engine.cast("<expr>"),
            self.engine.cast("coalesce(<expr>, <default>)")
        );

        // sqlc reports `coalesce(x, ...)` as the column `x` but NOT NULL, and with the type of that first argument
        // whatever PostgreSQL resolves (`coalesce(qty, 1.5)` is numeric), so we cannot trust its type either.
        if column.not_null && self.source(column).is_some_and(|source| !source.not_null) {
            bail!(
                "column \"{described}\" has a type sqlc could not infer; add a cast, as {}",
                self.engine.cast("coalesce(<expr>, <default>)")
            )
        }
        if self.engine.is_untyped_expression(column) {
            bail!(
                "column \"{described}\" is an expression sqlc typed by its form, not its operands; \
                 add a cast, as {}",
                self.engine.cast("<expr>")
            )
        }
        Ok(Field {
            ty: self.field_type(column, false, &format!("column \"{described}\""), &cast)?,
            comment: column.comment.clone(),
            name,
        })
    }

    /// The catalog column sqlc reports a result column is read from.
    fn source(&self, column: &plugin::Column) -> Option<&plugin::Column> {
        let table = column.table.as_ref()?;
        (self.catalog.schema(&table.schema)?.tables.iter())
            .find(|t| t.rel.as_ref().is_some_and(|rel| rel.name == table.name))?
            .columns
            .iter()
            .find(|c| c.name == column.original_name)
    }

    /// The type of `column`, which `what` describes in errors. A type without a verified Python type is an error, as
    /// annotating it `Any` would hide exactly the mistakes the type checkers are there to catch; for a type sqlc could
    /// not infer, the error suggests `cast`.
    ///
    /// sqlc types a column it marks `is_func_call` by an overload it guessed (`abs(numeric)` as `bigint`), and the
    /// other guesses are [`Engine::is_guess`]'s to tell.
    pub fn field_type(&self, column: &plugin::Column, is_parameter: bool, what: &str, cast: &str) -> Result<FieldType> {
        let data_type = column.data_type();
        let sql_enum = self.declared_enum(&data_type);

        // Six is PostgreSQL's own limit (MAXDIM) and no other engine has arrays, so a deeper array is no type to nest a
        // `list` for.
        let array_dims = match usize::try_from(column.array_dims) {
            Ok(dims) if dims <= 6 => dims,
            _ => bail!(
                "{what} is an array of {} dimensions, where an array has six at most",
                column.array_dims
            ),
        };

        // An override replaces the element's type, while the array around it is still the one the driver returns
        let element = match self.overridden(column, &data_type) {
            Some(_) if sql_enum.is_some() && array_dims > 0 => {
                bail!(
                    "{what} is an array of enum \"{data_type}\", whose elements a driver may return as the array's text; an override cannot replace them"
                )
            }
            Some(rule) => Type::Custom {
                annotation: rule.python_type.expr.clone(),
                json: (self.engine.scalar_type(column)).and_then(|ty| ty.json()),
            },
            None => {
                if column.is_func_call {
                    bail!(
                        "{what} is a function's result, which sqlc types by the function's first \
                         overload whatever its arguments (sum() of a numeric column as bigint); \
                         add a cast, as {cast}"
                    )
                }
                if self.engine.is_guess(column, is_parameter) {
                    bail!(
                        "{what} has a type sqlc could not infer; add a cast, as {cast}{}",
                        self.engine.analysis()
                    )
                }
                // A type the schema declares is the user's, even where its name is also one of PostgreSQL's or an
                // extension's, and a composite has no class yet
                let declared = self.declares(&data_type);
                let Some(element) = (sql_enum
                    .map(|(schema, sql_enum)| Type::Enum(self.class_name(schema, &sql_enum.name))))
                .or_else(|| (!declared).then(|| self.engine.scalar_type(column))?)
                .or_else(|| {
                    let name = self.engine.type_name(&data_type);
                    (!declared).then(|| self.backend.scalar_type(self.engine, &name))?
                }) else {
                    let hint = self.engine.unmapped(&data_type);
                    let default = format!("{}.", self.catalog.default_schema);
                    let named = data_type.strip_prefix(&default).unwrap_or(&data_type);
                    bail!(
                        "{what} has type \"{data_type}\", which has no verified Python mapping; \
                         name its Python type with an override (db_type \"{named}\"){hint}"
                    )
                };
                if let Some((_, sql_enum)) = sql_enum
                    && let Some(problem) = self.engine.enum_problem(column, &sql_enum.vals)
                {
                    bail!("{what} {problem}")
                }
                element
            }
        };

        if column.is_array && array_dims == 0 {
            bail!("{what} is an array, but sqlc reported no array depth for it")
        }
        if array_dims > 0
            && let Some(problem) = self.engine.array_problem(&data_type, is_parameter)
        {
            bail!("{what} is an array of \"{data_type}\", which {problem}")
        }
        if matches!(element, Type::Library { .. }) && array_dims > 0 {
            bail!("{what} is an array of \"{data_type}\", which has no verified Python mapping")
        }
        if matches!(element, Type::Enum(_)) && array_dims > 1 {
            bail!("{what} is a multi-dimensional array of enum \"{data_type}\", which has no verified Python mapping")
        }

        Ok(FieldType {
            as_text: array_dims > 0 && matches!(element, Type::Enum(_)) && self.backend.text_arrays(),
            element,
            array_dims,
            optional: !column.not_null,
        })
    }

    /// The enum, and its schema, that `data_type` names when the schema itself declares it. A column names its type
    /// `schema.name`, or bare for the default schema.
    ///
    /// TODO: Composite types get no class yet, so a column of one is as unknown as a type nothing declares.
    fn declared_enum(&self, data_type: &str) -> Option<(&'a plugin::Schema, &'a plugin::Enum)> {
        let (schema, name) = data_type.rsplit_once('.').unwrap_or(("", data_type));
        let schema = self.catalog.schema(schema)?;
        Some((schema, schema.enums.iter().find(|e| e.name == name)?))
    }

    /// Whether a schema of the catalog declares `data_type`, `schema.name` or bare for the default schema, as an enum or
    /// a composite.
    fn declares(&self, data_type: &str) -> bool {
        let (schema, name) = data_type.rsplit_once('.').unwrap_or(("", data_type));
        (self.catalog.schema(schema)).is_some_and(|schema| {
            schema.enums.iter().any(|e| e.name == name) || schema.composite_types.iter().any(|c| c.name == name)
        })
    }
}
