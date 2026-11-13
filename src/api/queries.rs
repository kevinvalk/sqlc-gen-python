//! The queries sqlc resolved, as the functions we will generate
//! (`docs/python-output.md` §6).
//!
//! Like [`super::models`], this decides everything once: names, types, and the
//! shape of each result. [`crate::render`] only writes it down.

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::path::Path;

use anyhow::{Context, Result, bail};
use heck::{ToSnakeCase, ToUpperCamelCase};
use ruff_python_stdlib::builtins::is_python_builtin;

use super::Problems;
use super::models::{self, Field, FieldType, Model, Repeats, Resolver};
use crate::config::Config;
use crate::engine::select::Item;
use crate::engine::{Bind, Engine, Sql};
use crate::plugin;
use crate::python::{self, Type};

#[cfg(test)]
#[path = "queries_spec.rs"]
mod spec;

/// The queries of one query file, which become one module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Module {
    /// The query file's stem: `invoices.sql` is the module `invoices`.
    pub name: String,
    pub queries: Vec<Query>,
}

/// One query, as a SQL constant, perhaps a class, and a function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    pub function: String,
    pub constant: String,
    pub comment: String,
    /// The statement as sqlc wrote it, cut at its placeholders.
    pub sql: Sql,
    /// The function's parameters: each name sqlc reported, once.
    pub params: Vec<Field>,
    /// A `:copyfrom`, an `:exec` over `rows`: its table, `schema` empty for
    /// the search path's, and its columns, one per parameter, which `COPY`
    /// writes into.
    pub copy_into: Option<CopyInto>,
    /// Every column of the result as the database returns it, embeds expanded.
    pub columns: Vec<Field>,
    pub command: Command,
    /// The `<Query>Params` class whose rows the function takes, if it runs its
    /// command once per row: a `:copyfrom`, or a batch (`:batchexec` is an
    /// `:exec` per row, `:batchone` a `:one`, `:batchmany` a `:many`).
    pub rows: Option<String>,
    /// The `<Query>Params` class the function takes its parameters as, if it
    /// takes more than the options' `max_params`.
    pub bundle: Option<String>,
    /// Whether a `:one` raises without a row, rather than return `None`: as
    /// the options say, but a statement that always returns one (`SELECT
    /// count(*) ...`) has none to lack, so it is its row's type.
    pub raises: bool,
}

impl Query {
    /// Whether the query reads at most one row each run: a `:one`, or a
    /// `:batchone`.
    pub fn reads_one(&self) -> bool {
        matches!(self.command, Command::One(_))
    }

    /// The classes the query defines: its `Params`, then its `Row`.
    pub fn classes(&self) -> impl Iterator<Item = &str> {
        let row = match &self.command {
            Command::One(Shape::Row(class, _)) | Command::Many(Shape::Row(class, _)) => Some(class),
            _ => None,
        };
        (self.rows.iter().chain(&self.bundle).chain(row)).map(String::as_str)
    }
}

/// A table and the columns a `:copyfrom` inserts, as SQL names them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyInto {
    pub schema: String,
    pub table: String,
    pub columns: Vec<String>,
}

/// What a query's function does with the statement it runs (`cmd`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Exec,
    ExecRows,
    ExecResult,
    /// Returns the id the `INSERT` generated.
    ExecLastId,
    /// Returns the only row; none, or more than one, raises.
    One(Shape),
    /// Returns every row, as a list.
    Many(Shape),
}

/// What one row of a result becomes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shape {
    /// The value of the only column.
    Scalar(FieldType),
    /// The model of the table the columns are exactly those of.
    Model(String),
    /// The `<Query>Row` class named here, of these fields.
    Row(String, Vec<RowField>),
}

/// One field of a `<Query>Row`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowField {
    Column(Field),
    /// `sqlc.embed(t)`: `t`'s model, built from the next `width` columns.
    /// If an outer join may leave `t` without a row, `missing` is the
    /// model's first NOT NULL column, NULL only then: the field is then
    /// `None`.
    Embed {
        name: String,
        model: String,
        width: usize,
        missing: Option<usize>,
    },
}

/// The modules of `queries`, in the order sqlc lists them.
pub fn modules(
    queries: &[plugin::Query],
    resolver: &Resolver,
    models: &[Model],
    config: &Config,
    problems: &mut Problems,
) -> Vec<Module> {
    let mut modules: Vec<Module> = Vec::new();
    // The functions and classes of each module, and the query each is for.
    let mut defined = HashMap::new();
    let mut misnamed = HashSet::new();
    // The file each module is of.
    let mut files: HashMap<String, &String> = HashMap::new();
    for query in queries {
        let file = &query.filename;
        let name = match module_name(file) {
            Ok(name) => Some(name),
            // A file's name is one problem, however many queries it holds.
            Err(error) => {
                if misnamed.insert(file) {
                    problems.note(error);
                }
                None
            }
        };
        let what = format!("{file}: query \"{}\"", query.name);
        let resolved = problems.take(&what, resolve(query, resolver, models, config));
        let (Some(name), Some(resolved)) = (name, resolved) else {
            continue;
        };
        if let Some(other) = files.insert(name.clone(), file)
            && other != file
        {
            problems.note(anyhow::anyhow!(
                "{other} and {file} both become the module \"{name}.py\"; rename one"
            ));
        }
        for python in std::iter::once(resolved.function.as_str()).chain(resolved.classes()) {
            if let Some(first) = defined.insert((name.clone(), python.to_owned()), &query.name) {
                problems.note(anyhow::anyhow!(
                    "{file}: \"{first}\" and \"{}\" both become the Python name \"{python}\"",
                    query.name
                ));
            }
        }
        match modules.iter_mut().find(|module| module.name == name) {
            Some(module) => module.queries.push(resolved),
            None => modules.push(Module {
                name,
                queries: vec![resolved],
            }),
        }
    }
    modules
}

/// The module a query file becomes, named after its stem, each `-` a `_`,
/// unless that is no module name, or would replace a module the plugin
/// writes itself.
fn module_name(file: &str) -> Result<String> {
    let stem = (Path::new(file).file_stem())
        .and_then(OsStr::to_str)
        .unwrap_or("")
        .replace('-', "_");
    if ["models", "__init__"].contains(&stem.to_lowercase().as_str()) {
        bail!(
            "{file}: query file \"{file}\" would overwrite the generated module \"{stem}.py\"; rename it"
        );
    }
    if !python::is_module_name(&stem) {
        bail!("{file}: \"{stem}\" is not a lowercase Python module name; rename the file");
    }
    Ok(stem)
}

fn resolve(
    query: &plugin::Query,
    resolver: &Resolver,
    models: &[Model],
    config: &Config,
) -> Result<Query> {
    let cmd = query.cmd.as_str();
    let class = query.name.to_upper_camel_case();
    // A batch runs a command once per row, on PostgreSQL alone, as pgx does.
    let (cmd, batch) = match cmd.strip_prefix(":batch") {
        Some(_) if resolver.engine != Engine::PostgreSql => {
            bail!("{cmd} is PostgreSQL only, as sqlc's pgx; use :copyfrom or a :exec per row")
        }
        Some(_) if query.params.is_empty() => {
            bail!("{cmd} has no parameters to run it per row with")
        }
        Some("exec") => (":exec", true),
        Some("one") => (":one", true),
        Some("many") => (":many", true),
        _ => (cmd, cmd == ":copyfrom"),
    };
    let params_class = format!("{class}Params");
    let rows = batch.then(|| params_class.clone());
    let (command, columns) = match cmd {
        ":exec" => (Command::Exec, Vec::new()),
        ":execrows" => (Command::ExecRows, Vec::new()),
        ":execresult" => (Command::ExecResult, Vec::new()),
        ":copyfrom" => (Command::Exec, Vec::new()),
        ":one" | ":many" => {
            let (fields, columns) = columns(query, resolver, models)?;
            let shape = match fields.as_slice() {
                [] => bail!(
                    "{cmd} has no result columns in sqlc's report; if the query returns rows, \
                     list them with casts (SELECT n::int, s::text FROM ...), otherwise use :exec"
                ),
                [RowField::Column(field)] => Shape::Scalar(field.ty.clone()),
                _ => row_shape(
                    models,
                    &query.columns,
                    fields,
                    resolver.catalog,
                    class + "Row",
                ),
            };
            match cmd {
                ":one" => (Command::One(shape), columns),
                _ => (Command::Many(shape), columns),
            }
        }
        ":execlastid" if resolver.engine.has_last_insert_id() => (Command::ExecLastId, Vec::new()),
        ":execlastid" => bail!(
            ":execlastid is not supported on {}; use INSERT ... RETURNING with :one",
            resolver.engine.name()
        ),
        _ => bail!("{cmd} is not a command sqlc-gen-python knows"),
    };
    let uses = params(&query.params, resolver)?;
    // PostgreSQL's `$N` is its parameter's number. MySQL's and SQLite's `?`
    // are numbered by sqlc's order of the parameters, as sqlc-gen-go binds
    // them: the numbers sqlc reports there may not be the text's.
    let binds: Vec<Bind> = (query.params.iter().zip(&uses).enumerate())
        .map(|(i, (param, field))| {
            let column = param.column.as_ref();
            Bind {
                number: match resolver.engine {
                    Engine::PostgreSql => param.number,
                    Engine::MySql | Engine::Sqlite => i as i32 + 1,
                },
                name: &field.name,
                slice: (column.filter(|c| c.is_sqlc_slice)).map(|c| c.name.as_str()),
            }
        })
        .collect();
    let sql = resolver.engine.sql(&query.text, &binds)?;
    let mut params: Vec<Field> = Vec::new();
    for field in uses {
        if !params.iter().any(|p| p.name == field.name) {
            params.push(field);
        }
    }

    // A module-level function also takes the trailing `_` for a builtin it
    // would hide (`Any` is `any_`, ruff A001).
    let function = models::names(&[query.name.as_str()], Repeats::Forbidden)?.remove(0);
    let function = match is_python_builtin(&function, 10, false) {
        true => function + "_",
        false => function,
    };
    // More parameters than the options take as keywords: one record of them.
    let bundle =
        (!batch && config.max_params.is_some_and(|max| params.len() > max)).then_some(params_class);
    let resolved = Query {
        constant: format!("_{}", function.to_uppercase()),
        function,
        comment: (query.comments.iter())
            .map(|line| line.strip_prefix(' ').unwrap_or(line))
            .collect::<Vec<_>>()
            .join("\n"),
        sql,
        params,
        copy_into: copy_into(query),
        columns,
        raises: config.raise_on_none || resolver.engine.single_row(&query.text),
        bundle,
        command,
        rows,
    };
    config.backend.check(resolver.engine, &resolved)?;
    Ok(resolved)
}

/// What a `:copyfrom` inserts into: the table sqlc reports, and each
/// parameter's column of it.
fn copy_into(query: &plugin::Query) -> Option<CopyInto> {
    let table = query
        .insert_into_table
        .as_ref()
        .filter(|_| query.cmd == ":copyfrom")?;
    let columns = (query.params.iter())
        .map(|param| {
            let column = param.column.as_ref()?;
            Some(match column.original_name.as_str() {
                "" => column.name.clone(),
                original => original.to_string(),
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(CopyInto {
        schema: table.schema.clone(),
        table: table.name.clone(),
        columns,
    })
}

/// A query's parameters, in sqlc's order, one per placeholder: a name the
/// user chose names one parameter wherever it is used, which on MySQL sqlc
/// reports once per use.
fn params(params: &[plugin::Parameter], resolver: &Resolver) -> Result<Vec<Field>> {
    let columns: Vec<plugin::Column> = (params.iter())
        .map(|param| param.column.clone().unwrap_or_default())
        .collect();
    // sqlc's Go codegen numbers parameters that spell one name, but a name
    // the user chose (`sqlc.arg`, `@name`) must bind the placeholder it was
    // written at, not take, or give another, a suffix.
    // A name the user chose is the identifier after `@`. sqlc names the
    // operand of `@` after its parts when it is an expression, which `@`
    // takes when an operator binds tighter (`@day::date AT TIME ZONE 'UTC'`
    // is `@(pg_catalog.timezone('UTC', day::date))`), and may then have
    // replaced all of it with the placeholder.
    if let Some(column) = (columns.iter()).find(|c| {
        c.is_named_param
            && (!resolver.engine.is_identifier(&c.name) || c.name.starts_with("pg_catalog"))
    }) {
        bail!(
            "sqlc named a parameter \"{}\" after an expression, not a name: an operator that \
             binds tighter than @, such as AT TIME ZONE or +, made @ take the expression, and \
             sqlc may have replaced all of it; write sqlc.arg(name), or parenthesise (@name::type)",
            column.name
        );
    }
    // Each use of a name the user chose after the first: which use it repeats.
    let mut chosen: HashMap<String, usize> = HashMap::new();
    let repeats: Vec<Option<usize>> = (columns.iter().enumerate())
        .map(|(i, c)| match c.is_named_param && !c.name.is_empty() {
            true => match chosen.get(&c.name) {
                Some(&first) => Some(first),
                None => chosen.insert(c.name.clone(), i).and(None),
            },
            false => None,
        })
        .collect();
    let mut seen: HashMap<String, &plugin::Column> = HashMap::new();
    for (column, _) in
        (columns.iter().zip(&repeats)).filter(|(c, r)| !c.name.is_empty() && r.is_none())
    {
        let name = resolver.spelling(&column.name).to_snake_case();
        if let Some(first) = seen.insert(name.clone(), column)
            && (first.is_named_param || column.is_named_param)
        {
            bail!(
                "parameters \"{}\" and \"{}\" both become the Python name \"{name}\"; rename one",
                first.name,
                column.name,
            );
        }
    }
    let firsts: Vec<(usize, &str)> = (columns.iter().zip(&repeats).enumerate())
        .filter(|(_, (_, repeat))| repeat.is_none())
        .map(|(i, (c, _))| (i, resolver.spelling(&c.name)))
        .collect();
    let spellings: Vec<&str> = firsts.iter().map(|&(_, spelling)| spelling).collect();
    let mut names: Vec<String> = vec![String::new(); columns.len()];
    for ((i, _), name) in firsts
        .iter()
        .zip(models::names(&spellings, Repeats::Numbered)?)
    {
        names[*i] = name;
    }
    for (i, repeat) in repeats.iter().enumerate() {
        if let Some(first) = repeat {
            names[i] = names[*first].clone();
        }
    }

    let mut fields: Vec<Field> = Vec::new();
    for (((name, column), param), repeat) in
        names.into_iter().zip(columns).zip(params).zip(&repeats)
    {
        let what = format!(
            "parameter {} (\"{name}\")",
            resolver.engine.placeholder(param.number)
        );
        if column.is_sqlc_slice && resolver.engine == Engine::PostgreSql {
            bail!("sqlc.slice is MySQL/SQLite only; on PostgreSQL use = ANY(@{name}::type[])")
        }
        // The cast goes on the placeholder: a parameter cannot be coalesced.
        let cast = format!(
            "{}, or {} if it can be NULL",
            resolver.engine.cast(&match resolver.engine {
                Engine::PostgreSql => format!("@{name}"),
                _ => format!("sqlc.arg({name})"),
            }),
            resolver.engine.cast(&format!("sqlc.narg('{name}')"))
        );
        let mut ty = resolver.field_type(&column, &what, &cast)?;
        if column.is_sqlc_slice {
            if ty.array_dims > 0 {
                bail!("{what} is a sqlc.slice of an array, which binds no placeholder per item");
            }
            // The list is never NULL: its items are each bound.
            ty.array_dims = 1;
            ty.optional = false;
        }
        // Every use must take one type; one that admits NULL makes it
        // optional, as sqlc reports each use's own column.
        if let Some(first) = repeat.map(|first| &mut fields[first]) {
            if (&first.ty.element, first.ty.array_dims) != (&ty.element, ty.array_dims) {
                bail!(
                    "{what} is used as {} and as {}; cast each use to one type",
                    first.ty.describe(crate::python::Scope::Queries),
                    ty.describe(crate::python::Scope::Queries)
                );
            }
            first.ty.optional |= ty.optional;
            ty.optional = first.ty.optional;
        }
        if matches!(ty.element, Type::Json(_)) && ty.array_dims > 0 {
            bail!(
                "{what} is an array of \"{}\", which has no verified Python mapping",
                column.data_type()
            );
        }
        fields.push(Field {
            name,
            comment: String::new(),
            ty,
        });
    }
    Ok(fields)
}

/// A result's fields, and its columns as the database returns them.
fn columns(
    query: &plugin::Query,
    resolver: &Resolver,
    models: &[Model],
) -> Result<(Vec<RowField>, Vec<Field>)> {
    // Where sqlc's report is not enough: a `count(...)`'s type is the
    // engine's whatever it counts, which sqlc reports as a guess like any
    // call's; and a scalar subquery is NULL when it finds no row, which sqlc
    // reports NOT NULL.
    let items = resolver.engine.items(&query.text, query.columns.len());
    let columns: Vec<plugin::Column> = (query.columns.iter().enumerate())
        .map(|(i, column)| match items.as_ref().map(|items| &items[i]) {
            Some(Item::Call(name)) if name == "count" && column.is_func_call => plugin::Column {
                is_func_call: false,
                r#type: Some(plugin::Identifier {
                    name: resolver.engine.count_type().into(),
                    ..Default::default()
                }),
                ..column.clone()
            },
            Some(Item::Subquery { row: false }) => plugin::Column {
                not_null: false,
                ..column.clone()
            },
            _ => column.clone(),
        })
        .collect();
    let columns = columns.as_slice();
    let outer = resolver.engine.outer_joined(&query.text);
    let embeds: Vec<Option<&Model>> = (columns.iter())
        .map(|column| {
            let Some(table) = column.embed_table.as_ref().filter(|t| !t.name.is_empty()) else {
                return Ok(None);
            };
            (models
                .iter()
                .find(|model| table.is(&model.table, resolver.catalog)))
            .map(Some)
            .with_context(|| format!("sqlc.embed({}) names no table", table.name))
        })
        .collect::<Result<_>>()?;
    let spellings: Vec<&str> = (columns.iter().zip(&embeds))
        .map(|(column, embed)| {
            embed.map_or(resolver.spelling(&column.name), |model| model.name.as_str())
        })
        .collect();

    let mut fields = Vec::new();
    let mut flat = Vec::new();
    for ((name, column), embed) in models::names(&spellings, Repeats::Numbered)?
        .into_iter()
        .zip(columns)
        .zip(embeds)
    {
        match embed {
            Some(model) => {
                let table = column.embed_table.as_ref().map_or("", |t| t.name.as_str());
                let outer = (outer.as_ref()).is_none_or(|tables| {
                    tables
                        .iter()
                        .any(|joined| joined.eq_ignore_ascii_case(table))
                });
                let missing = match outer {
                    false => None,
                    true => Some(
                        (model.fields.iter().position(|field| !field.ty.optional)).with_context(
                            || {
                                format!(
                                    "sqlc.embed({table}) of an outer join, of whose columns none \
                                     is NOT NULL to tell a missing row by; select its columns \
                                     instead"
                                )
                            },
                        )?,
                    ),
                };
                flat.extend(model.fields.iter().cloned().map(|mut field| {
                    field.ty.optional |= outer;
                    field
                }));
                fields.push(RowField::Embed {
                    missing,
                    name,
                    model: model.name.clone(),
                    width: model.fields.len(),
                });
            }
            None => {
                let field = resolver.field(name, column)?;
                flat.push(field.clone());
                fields.push(RowField::Column(field));
            }
        }
    }
    Ok((fields, flat))
}

/// The shape of a row of several columns: the model of the table they are
/// exactly the columns of, as sqlc-gen-go reuses one (at least two columns,
/// all of one table, with that table's names, order, types and nullability),
/// or else a `<Query>Row`.
fn row_shape(
    models: &[Model],
    columns: &[plugin::Column],
    fields: Vec<RowField>,
    catalog: &plugin::Catalog,
    class: String,
) -> Shape {
    // Fewer than two columns never get here: one is a scalar, and one embed
    // is no column of the table.
    let is_model = |model: &&Model| {
        (columns.iter()).all(|c| (c.table.as_ref()).is_some_and(|t| t.is(&model.table, catalog)))
            && model.fields.len() == fields.len()
            && (model.fields.iter().zip(&fields)).all(|(a, b)| match b {
                RowField::Column(b) => a.name == b.name && a.ty == b.ty,
                RowField::Embed { .. } => false,
            })
    };
    match models.iter().find(is_model) {
        Some(model) => Shape::Model(model.name.clone()),
        None => Shape::Row(class, fields),
    }
}
