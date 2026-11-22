use std::cell::RefCell;
use std::collections::BTreeSet;
use std::sync::LazyLock;

use anyhow::{Result, bail};
use heck::ToUpperCamelCase;

use super::Problems;
use super::inflection::singular;
use super::models::{Field, FieldType};
use super::names::{Binding, Repeats, names};
use crate::config::{Config, Override};
use crate::engine::Engine;
use crate::plugin;
use crate::python::Type;
use crate::sqlc_typing;

#[cfg(test)]
#[path = "resolver_spec.rs"]
mod spec;

/// Reads the Python type of each column of one catalog, by its engine's type map and the user's overrides, and the
/// Python spelling of each SQL name, by the user's renames.
pub struct Resolver<'a> {
    pub engine: Engine,
    pub catalog: &'a plugin::Catalog,
    pub config: &'a Config,
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
            config,
            used: RefCell::default(),
        })
    }

    /// Fails for a rename key or an override that matched nothing.
    pub fn check_used(&self) -> Result<()> {
        let (keys, overrides) = &*self.used.borrow();
        if let Some(key) = self.config.rename.keys().find(|key| !keys.contains(key.as_str())) {
            bail!("rename: \"{key}\" names no table, type, column or parameter of the queries");
        }
        // Entries often share one list of overrides (a YAML anchor), so one of them may well not use an override,
        // which is then no typo as long as it names a type or a column of the schema
        let rules = &self.config.overrides;
        if let Some(i) = (0..rules.len()).find(|i| !overrides.contains(i) && !self.in_schema(&rules[*i])) {
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
                (table.columns.iter()).any(|column| self.names_column(named, &schema.name, name, &column.name))
            })
        };
        rule.db_type.as_deref().is_some_and(names_type) || rule.column.as_deref().is_some_and(names_column)
    }

    /// Whether an override's `ty` names `data_type`: by any spelling of the engine's ([`Engine::type_name`]), or bare
    /// for PostgreSQL's own, the default schema's, or an extension's, which no schema declares and may be in any
    /// (`vector` is `extensions.vector`). A bare name of PostgreSQL's own is never another schema's type, which is a
    /// domain of the user's (`archive.money`).
    fn names_type(&self, ty: &str, data_type: &str) -> bool {
        // sqlc reports one type by several names (a `bigint` column as `pg_catalog.int8`, a `bigserial` one as
        // `bigserial`), and an override that reached only one of them would give one database type two Python types.
        let own = |name: &str| self.engine.type_name(&self.plain(name));
        let is_extensions = !ty.contains('.') && !self.declares(data_type) && !self.engine.is_own_type(&own(ty));
        let bare = |name: &str| {
            name.rsplit_once('.')
                .map_or(name, |(_, name)| name)
                .to_ascii_lowercase()
        };
        own(ty) == own(data_type) || (is_extensions && bare(data_type) == ty.to_ascii_lowercase())
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
        let (key, renamed) = self.config.rename.get_key_value(key)?;
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

    /// The override that names `column` itself.
    fn column_override(&self, column: &plugin::Column) -> Option<&'a Override> {
        let table = column.table.as_ref()?;
        let name = match column.original_name.as_str() {
            "" => &column.name,
            original => original,
        };
        let schema = match table.schema.as_str() {
            "" => &self.catalog.default_schema,
            schema => schema,
        };
        self.first_override(|rule| {
            (rule.column.as_deref()).is_some_and(|named| self.names_column(named, schema, &table.name, name))
        })
    }

    /// Whether an override's `named` is `schema.table.column`, or a bare `table.column` of the default schema.
    fn names_column(&self, named: &str, schema: &str, table: &str, column: &str) -> bool {
        named == format!("{schema}.{table}.{column}")
            || (schema == self.catalog.default_schema && named == format!("{table}.{column}"))
    }

    /// The first override that `matches`, which we then note as used.
    fn first_override(&self, matches: impl Fn(&Override) -> bool) -> Option<&'a Override> {
        let i = self.config.overrides.iter().position(matches)?;
        self.used.borrow_mut().1.insert(i);
        Some(&self.config.overrides[i])
    }

    /// The class name of a table, singular unless the user opted out.
    pub fn model_name(&self, schema: &plugin::Schema, table: &str) -> String {
        // We rename by the table's name alone and never by its singular, as that may well be a column's name
        if let Some(renamed) = self.renamed_class(schema, table) {
            return renamed.into();
        }

        match self.config.models.exact_table_names {
            true => self.spelt(schema, table),
            false => self.spelt(schema, &singular(table)),
        }
    }

    /// A table's columns as fields, in order, leaving out each one with a problem after noting it under `what`.
    pub fn fields(&self, columns: &[plugin::Column], what: &str, problems: &mut Problems) -> Vec<Field> {
        let spellings: Vec<&str> = columns.iter().map(|c| self.spelling(&c.name)).collect();
        let names = problems.take(
            what,
            names(&spellings, Repeats::Forbidden, Binding::Field, self.config.records),
        );
        (names.unwrap_or_default().into_iter().zip(columns))
            .filter_map(|(name, column)| problems.take(what, self.field(name, column, &sqlc_typing::Column::default())))
            .collect()
    }

    pub(super) fn field(&self, name: String, reported: &plugin::Column, facts: &sqlc_typing::Column) -> Result<Field> {
        let column = facts.apply(reported);
        let described = if column.name.is_empty() { &name } else { &column.name };
        let what = format!("column \"{described}\"");
        let ty = match sqlc_typing::expression_problem(self.engine, &column, self.catalog, facts, &name) {
            Some(problem) => self.unknown(&column, problem)?,
            None => self.field_type(&column, None, &what, facts)?,
        };
        Ok(Field {
            ty,
            comment: column.comment,
            name,
        })
    }

    /// The type of `column`, which `what` describes in errors. A type without a verified Python type is
    /// [`Self::unknown`], as annotating it `Any` unasked would hide exactly the mistakes the type checkers are there to
    /// catch.
    pub(super) fn field_type(
        &self,
        column: &plugin::Column,
        parameter: Option<&str>,
        what: &str,
        facts: &sqlc_typing::Column,
    ) -> Result<FieldType> {
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

        // An override naming the column wins over all sqlc reports of it, as on SQLite it is how a column declared
        // without a type gets one. One naming a type only reaches a type sqlc read: its guess is spelled as a type too
        // (`bigint` for the `sum` of a numeric column), and is no column of that type.
        let rule = match self.column_override(column) {
            Some(rule) => Some(rule),
            None => match sqlc_typing::allows_type_override(self.engine, column, parameter, what) {
                Ok(true) => self.first_override(|rule| {
                    rule.db_type
                        .as_deref()
                        .is_some_and(|ty| self.names_type(ty, &data_type))
                }),
                Ok(false) => None,
                Err(problem) => return self.unknown(column, problem),
            },
        };

        // An override replaces the element's type, while the array around it is still the one the driver returns
        let declared = self.declares(&data_type);
        let mapped = || {
            if declared {
                return None;
            }
            self.engine.scalar_type(column).or_else(|| {
                let name = self.engine.type_name(&data_type);
                self.config.backend.scalar_type(self.engine, &name)
            })
        };
        let element = match (rule, sql_enum) {
            (Some(_), Some(_)) if array_dims > 0 => {
                bail!(
                    "{what} is an array of enum \"{data_type}\", whose elements a driver may return as the \
                     array's text; an override cannot replace them"
                )
            }
            (Some(rule), _) => {
                // A driver returns an enum label. For a type without a verified mapping only object is sound,
                // so the user's decoder has to narrow it before use.
                let wire = match (sql_enum.is_some(), mapped()) {
                    (true, _) => Type::STR,
                    (_, Some(wire)) => wire,
                    (_, None) if array_dims == 0 || rule.from_db.is_none() => Type::OBJECT,
                    _ => bail!(
                        "{what} is an array of \"{data_type}\", which has no verified list representation; \
                         cast it to an array type the driver returns as a list"
                    ),
                };
                Type::Override {
                    annotation: rule.python_type.expr.clone(),
                    wire: Box::new(wire),
                    converters: rule.to_db.as_ref().zip(rule.from_db.as_ref()).map(|(to, from)| {
                        Box::new(crate::python::Converters {
                            to_db: to.0.expr.clone(),
                            from_db: from.0.expr.clone(),
                        })
                    }),
                }
            }
            (None, Some((schema, sql_enum))) => {
                let class = self.class_name(schema, &sql_enum.name);
                match sqlc_typing::is_set_column(self.engine, self.catalog, column, &sql_enum.vals) {
                    Ok(true) => Type::STR,
                    Ok(false) => Type::Enum(class),
                    Err(problem) => return self.unknown(column, format!("{what} {problem}")),
                }
            }
            (None, None) => {
                // Composites have no verified mapping, even when their name matches a builtin or extension type.
                let Some(element) = mapped() else {
                    let hint = sqlc_typing::unmapped_hint(self.engine, &data_type);
                    let default = format!("{}.", self.catalog.default_schema);
                    let named = data_type.strip_prefix(&default).unwrap_or(&data_type);
                    return self.unknown(
                        column,
                        format!(
                            "{what} has type \"{data_type}\", which has no verified Python mapping; \
                             name its Python type with an override (db_type \"{named}\"){hint}"
                        ),
                    );
                };
                element
            }
        };

        if column.is_array && array_dims == 0 {
            bail!("{what} is an array, but sqlc reported no array depth for it")
        }
        if array_dims > 0
            && let Some(problem) = self.engine.array_problem(&data_type, parameter.is_some())
        {
            return self.unknown(
                column,
                format!("{what} is an array of \"{data_type}\", which {problem}"),
            );
        }
        if matches!(element.wire(), Type::Library { .. }) && array_dims > 0 {
            let problem = format!("{what} is an array of \"{data_type}\", which has no verified Python mapping");
            return self.unknown(column, problem);
        }
        if matches!(element, Type::Enum(_)) && array_dims > 1 {
            let problem = format!(
                "{what} is a multi-dimensional array of enum \"{data_type}\", which has no verified Python mapping"
            );
            return self.unknown(column, problem);
        }

        Ok(FieldType {
            collects_nulls: facts.nullable_elements(self.engine, array_dims),
            as_text: array_dims > 0 && matches!(element, Type::Enum(_)) && self.config.backend.text_arrays(),
            element,
            array_dims,
            optional: !column.not_null,
        })
    }

    /// What we make of a column or parameter of a type we cannot verify: the error that says why, or the annotation the
    /// user accepts for every such type (`unknown_type`). That stands for the whole column, as a driver may not even
    /// return a list for an array of it.
    pub fn unknown(&self, column: &plugin::Column, problem: String) -> Result<FieldType> {
        let Some(unknown) = self.config.unknown_type else {
            bail!("{problem}; or set unknown_type: Any to leave it unchecked");
        };
        Ok(FieldType {
            element: unknown.ty(),
            array_dims: 0,
            optional: !column.not_null,
            collects_nulls: false,
            as_text: false,
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

    /// Whether a schema of the catalog declares `data_type`, `schema.name` or bare for the default schema, as an enum
    /// or a composite.
    fn declares(&self, data_type: &str) -> bool {
        let (schema, name) = data_type.rsplit_once('.').unwrap_or(("", data_type));
        (self.catalog.schema(schema)).is_some_and(|schema| {
            schema.enums.iter().any(|e| e.name == name) || schema.composite_types.iter().any(|c| c.name == name)
        })
    }
}
