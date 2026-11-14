//! Reading one catalog: the Python type of each column, by the engine's type
//! map and the user's overrides, and the Python spelling of each SQL name, by
//! the user's renames.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use anyhow::{Result, bail};
use heck::ToUpperCamelCase;

use super::Problems;
use super::inflection::singular;
use super::models::{Field, FieldType};
use super::names::{Repeats, names};
use crate::backend::Backend;
use crate::config::{Config, ModelConfig, Override};
use crate::engine::Engine;
use crate::plugin;
use crate::python::Type;

#[cfg(test)]
#[path = "resolver_spec.rs"]
mod spec;

/// Reads types and names out of one catalog, by its engine's type map and
/// the user's overrides and renames.
pub struct Resolver<'a> {
    pub engine: Engine,
    pub catalog: &'a plugin::Catalog,
    overrides: &'a [Override],
    rename: &'a BTreeMap<String, String>,
    /// Whether sqlc analysed the queries on a database.
    database: bool,
    backend: Backend,
    /// The rename keys and overrides (by index) something matched: any
    /// other is a typo, which would silently do nothing.
    used: RefCell<(BTreeSet<&'a str>, BTreeSet<usize>)>,
}

impl<'a> Resolver<'a> {
    /// The resolver of the catalog in `request`, for its engine, as `config`
    /// overrides and renames.
    pub fn new(request: &'a plugin::GenerateRequest, config: &'a Config) -> Result<Self> {
        static EMPTY: LazyLock<plugin::Catalog> = LazyLock::new(plugin::Catalog::default);
        Ok(Self {
            engine: Engine::of(request)?,
            catalog: request.catalog.as_ref().unwrap_or(&EMPTY),
            overrides: &config.overrides,
            rename: &config.rename,
            database: config.database_analysis,
            backend: config.backend,
            used: RefCell::default(),
        })
    }

    /// An error for a rename key or an override that matched nothing.
    pub fn check_used(&self) -> Result<()> {
        let (keys, overrides) = &*self.used.borrow();
        if let Some(key) = self.rename.keys().find(|key| !keys.contains(key.as_str())) {
            bail!("rename: \"{key}\" names no table, type, column or parameter of the queries");
        }
        if let Some(i) = (0..self.overrides.len()).find(|i| !overrides.contains(i)) {
            bail!("overrides[{i}]: matches no column or parameter of the queries");
        }
        Ok(())
    }

    /// The rename of `key`, if any, noted as used.
    fn rename(&self, key: &str) -> Option<&'a str> {
        let (key, renamed) = self.rename.get_key_value(key)?;
        self.used.borrow_mut().0.insert(key);
        Some(renamed)
    }

    /// The name SQL name `sql` is spelt with in Python: its `rename`, or
    /// itself.
    pub fn spelling<'s>(&'s self, sql: &'s str) -> &'s str {
        self.rename(sql).unwrap_or(sql)
    }

    /// The class `name`, declared in `schema`, is renamed to, if any: by
    /// `schema.name`, or bare in the default schema.
    fn renamed_class(&self, schema: &plugin::Schema, name: &str) -> Option<&'a str> {
        (self.rename(&format!("{}.{name}", schema.name)))
            .or_else(|| (schema.name == self.catalog.default_schema).then(|| self.rename(name))?)
    }

    /// The class name for something declared in `schema`, unless renamed.
    /// Objects in the default schema keep their bare name; everything else is
    /// qualified, so `analytics.event` becomes `AnalyticsEvent`.
    pub fn class_name(&self, schema: &plugin::Schema, name: &str) -> String {
        match self.renamed_class(schema, name) {
            Some(renamed) => renamed.into(),
            None => self.spelt(schema, name),
        }
    }

    /// `name`, declared in `schema`, as a class name: qualified unless the
    /// default schema's.
    fn spelt(&self, schema: &plugin::Schema, name: &str) -> String {
        match schema.name == self.catalog.default_schema {
            true => name.to_upper_camel_case(),
            false => format!("{}_{name}", schema.name).to_upper_camel_case(),
        }
    }

    /// The override of `column`, of type `data_type`, if any: one naming
    /// the column itself first, then one naming its type.
    fn overridden(&self, column: &plugin::Column, data_type: &str) -> Option<&'a Override> {
        // A type as an override names it: bare for PostgreSQL's own or the
        // default schema's.
        let default = format!("{}.", self.catalog.default_schema);
        let plain = |name: &str| {
            let name = name.to_ascii_lowercase();
            (name
                .strip_prefix("pg_catalog.")
                .or_else(|| name.strip_prefix(&default)))
            .map(String::from)
            .unwrap_or(name)
        };
        let of = |rule: &&Override| {
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
        let i = (self.overrides.iter().position(|rule| of(&rule).is_some())).or_else(|| {
            (self.overrides.iter()).position(|rule| {
                rule.db_type
                    .as_deref()
                    .is_some_and(|ty| plain(ty) == plain(data_type))
            })
        })?;
        self.used.borrow_mut().1.insert(i);
        Some(&self.overrides[i])
    }

    /// The class name for a table, singularised unless the user opted out.
    pub fn model_name(&self, schema: &plugin::Schema, table: &str, config: &ModelConfig) -> String {
        if let Some(renamed) = self.renamed_class(schema, table) {
            return renamed.into();
        }
        // Renamed by the table's name alone: never by its singular, which
        // may well be a column's.
        match config.is_inflection_excluded(table) {
            true => self.spelt(schema, table),
            false => self.spelt(schema, &singular(table)),
        }
    }

    /// A table's columns, as fields in column order, each one's problem
    /// noted under `what` instead.
    pub fn fields(
        &self,
        columns: &[plugin::Column],
        what: &str,
        problems: &mut Problems,
    ) -> Vec<Field> {
        let spellings: Vec<&str> = columns.iter().map(|c| self.spelling(&c.name)).collect();
        let names = problems.take(what, names(&spellings, Repeats::Forbidden));
        (names.unwrap_or_default().into_iter().zip(columns))
            .filter_map(|(name, column)| problems.take(what, self.field(name, column)))
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
            "{}, or {} if it can be NULL, as sqlc reports every cast NOT NULL",
            self.engine.cast("<expr>"),
            self.engine.cast("coalesce(<expr>, <default>)")
        );
        // sqlc reports `coalesce(x, ...)` as the column `x`, but NOT NULL: the
        // type is the first argument's, whatever PostgreSQL resolves
        // (`coalesce(qty, 1.5)` is numeric), so it is no type to trust either.
        if column.not_null
            && !self.database
            && self.source(column).is_some_and(|source| !source.not_null)
        {
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
        let sql_enum = self.declared_enum(&data_type);
        let array_dims = column.array_dims as usize;
        // An override replaces the element's type, and its array is still
        // the array the driver returns.
        let element = match self.overridden(column, &data_type) {
            Some(_) if sql_enum.is_some() && array_dims > 0 => {
                bail!(
                    "{what} is an array of enum \"{data_type}\", whose elements a driver may return as the array's text; an override cannot replace them"
                )
            }
            Some(rule) => Type::Custom {
                annotation: rule.python_type.expr.clone(),
                json: match self.engine.scalar_type(column) {
                    Some(Type::Json(json)) => Some(json),
                    _ => None,
                },
            },
            None => {
                // On a database, PostgreSQL resolved a function call's type too.
                if column.is_func_call && !self.database {
                    bail!(
                        "{what} is a function's result, which sqlc types by the function's first \
                         overload whatever its arguments (sum() of a numeric column as bigint); \
                         add a cast, as {cast}{}",
                        self.engine.analyses()
                    )
                }
                if self.engine.is_guess(&data_type, self.database) {
                    bail!("{what} has a type sqlc could not infer; add a cast, as {cast}")
                }
                let Some(element) = (self.engine.scalar_type(column))
                    .or_else(|| self.backend.scalar_type(self.engine, &data_type))
                    .or_else(|| self.declared_type(&data_type))
                else {
                    let hint = self.engine.unmapped(&data_type);
                    let default = format!("{}.", self.catalog.default_schema);
                    let named = data_type.strip_prefix(&default).unwrap_or(&data_type);
                    bail!(
                        "{what} has type \"{data_type}\", which has no verified Python mapping; \
                         name its Python type with an override (db_type \"{named}\"){hint}"
                    )
                };
                if let Some(sql_enum) = sql_enum
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
            && let Some(problem) = self.engine.array_problem(&data_type)
        {
            bail!("{what} is an array of \"{data_type}\", which {problem}")
        }
        if matches!(element, Type::Library { .. }) && array_dims > 0 {
            bail!("{what} is an array of \"{data_type}\", which has no verified Python mapping")
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
        let (schema, _) = data_type.rsplit_once('.').unwrap_or(("", data_type));
        let sql_enum = self.declared_enum(data_type)?;
        Some(Type::Enum(
            self.class_name(self.catalog.schema(schema)?, &sql_enum.name),
        ))
    }

    /// The enum of the catalog `data_type` names, if any.
    fn declared_enum(&self, data_type: &str) -> Option<&'a plugin::Enum> {
        let (schema, name) = data_type.rsplit_once('.').unwrap_or(("", data_type));
        (self.catalog.schema(schema)?.enums.iter()).find(|e| e.name == name)
    }
}
