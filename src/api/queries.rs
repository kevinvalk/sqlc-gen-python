//! The queries sqlc resolved, as the functions we will generate
//! (`docs/python-output.md` §6).
//!
//! Like [`super::models`], this decides everything once: names, types, and the
//! shape of each result. [`crate::render`] only writes it down.

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::path::Path;

use anyhow::{Result, bail};
use heck::ToUpperCamelCase;
use ruff_python_ast as ast;
use ruff_python_stdlib::builtins::is_python_builtin;

use super::models::{Field, FieldType, Model};
use super::names::{self, Repeats};
use super::resolver::Resolver;
use super::{Problems, params, results};
use crate::config::Config;
use crate::engine::{Bind, Engine, Piece, Sql};
use crate::plugin;
use crate::python::vocabulary::{FROM_ROW, MODELS};
use crate::python::{self, Expression, Scope, int, name};

#[cfg(test)]
#[path = "queries_spec.rs"]
mod spec;

/// The queries of one query file, which become one module.
#[derive(Debug, Clone, PartialEq)]
pub struct Module {
    /// The query file's stem: `invoices.sql` is the module `invoices`.
    pub name: String,
    pub queries: Vec<Query>,
}

/// One query, as a SQL constant, perhaps a class, and a function.
#[derive(Debug, Clone, PartialEq)]
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
    /// writes into. None if the statement binds a parameter twice: `COPY`
    /// would then write one column for the two it inserts.
    pub copy_into: Option<CopyInto>,
    /// Every column of the result as the database returns it, embeds expanded.
    pub columns: Vec<Field>,
    pub command: Command,
    pub takes: Takes,
    /// Whether a `:one` raises without a row, rather than return `None`: as
    /// the options say, but a statement that always returns one (`SELECT
    /// count(*) ...`) has none to lack, so it is its row's type.
    pub raises: bool,
}

impl Query {
    /// The classes the query defines: its `Params`, then its `Row`.
    pub fn classes(&self) -> impl Iterator<Item = &str> {
        let row = match &self.command {
            Command::One(Shape::Row(class, _)) | Command::Many(Shape::Row(class, _)) => Some(class),
            _ => None,
        };
        (self.takes.class().into_iter()).chain(row.map(String::as_str))
    }
}

/// How a query's function takes its parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Takes {
    /// Each as a keyword.
    Keywords,
    /// All as one record of the `<Query>Params` class, `params`: more of them
    /// than the options' `max_params`.
    Record(String),
    /// `rows` of the `<Query>Params` class, its command run once per row: a
    /// `:copyfrom`, or a batch (`:batchexec` is an `:exec` per row,
    /// `:batchone` a `:one`, `:batchmany` a `:many`).
    Rows(String),
}

impl Takes {
    /// The `<Query>Params` class, if the function takes one.
    pub fn class(&self) -> Option<&str> {
        match self {
            Self::Keywords => None,
            Self::Record(class) | Self::Rows(class) => Some(class),
        }
    }

    /// Whether the function runs its command once per row.
    pub fn rows(&self) -> bool {
        matches!(self, Self::Rows(_))
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
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Exec,
    ExecRows,
    ExecResult,
    /// Returns the id the `INSERT` generated.
    ExecLastId,
    /// Returns the only row, or `None` without one unless the query
    /// [`raises`](Query::raises); a second row raises.
    One(Shape),
    /// Returns every row, as a list.
    Many(Shape),
}

/// What one row of a result becomes.
#[derive(Debug, Clone, PartialEq)]
pub enum Shape {
    /// The value of the only column.
    Scalar(FieldType),
    /// The model of the table the columns are exactly those of.
    Model(String),
    /// The `<Query>Row` class named here, of these fields.
    Row(String, Vec<RowField>),
}

impl Shape {
    /// The annotation of a row of this shape in its query module: `missing`,
    /// of one that may not be there.
    pub fn annotation(&self, missing: bool) -> ast::Expr {
        let class = match self {
            Self::Scalar(ty) => {
                let ty = FieldType {
                    optional: ty.optional || missing,
                    ..ty.clone()
                };
                return ty.annotation(Scope::Queries);
            }
            Self::Model(model) => MODELS.attr(model),
            Self::Row(class, _) => name(class),
        };
        match missing {
            true => class.or_none(),
            false => class,
        }
    }

    /// What `row`, as the database returns it, becomes: its only column, or
    /// its record.
    pub fn read(&self, row: impl Into<ast::Expr>, text_arrays: bool) -> ast::Expr {
        match self {
            Self::Scalar(ty) => ty.read(row.index(int(0)), Scope::Queries, text_arrays),
            record => record.annotation(false).attr(FROM_ROW).call([row.into()]),
        }
    }
}

/// One field of a `<Query>Row`.
#[derive(Debug, Clone, PartialEq)]
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

/// `query` as the function it becomes, or why it cannot be one.
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
    let (command, columns) = command(query, cmd, &class, resolver, models)?;
    let uses = params::params(&query.params, resolver)?;
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
    let placeholders = (sql.0.iter())
        .filter(|piece| !matches!(piece, Piece::Text(_)))
        .count();
    let mut params: Vec<Field> = Vec::new();
    for field in uses {
        if !params.iter().any(|p| p.name == field.name) {
            params.push(field);
        }
    }

    // A module-level function also takes the trailing `_` for a builtin it
    // would hide (`Any` is `any_`, ruff A001).
    let function = names::names(&[query.name.as_str()], Repeats::Forbidden)?.remove(0);
    let function = match is_python_builtin(&function, 10, false) {
        true => function + "_",
        false => function,
    };
    // Rows of them; or more than the options take as keywords, one record.
    let takes = match batch {
        true => Takes::Rows(params_class),
        false if config.max_params.is_some_and(|max| params.len() > max) => {
            Takes::Record(params_class)
        }
        false => Takes::Keywords,
    };
    let resolved = Query {
        constant: format!("_{}", function.to_uppercase()),
        function,
        comment: (query.comments.iter())
            .map(|line| line.strip_prefix(' ').unwrap_or(line))
            .collect::<Vec<_>>()
            .join("\n"),
        sql,
        params,
        copy_into: copy_into(query).filter(|into| placeholders == into.columns.len()),
        columns,
        raises: config.raise_on_none || resolver.engine.single_row(&query.text),
        takes,
        command,
    };
    config.backend.check(resolver.engine, &resolved)?;
    Ok(resolved)
}

/// The command `cmd` runs, with the columns of its result.
fn command(
    query: &plugin::Query,
    cmd: &str,
    class: &str,
    resolver: &Resolver,
    models: &[Model],
) -> Result<(Command, Vec<Field>)> {
    Ok(match cmd {
        ":exec" => (Command::Exec, Vec::new()),
        ":execrows" => (Command::ExecRows, Vec::new()),
        ":execresult" => (Command::ExecResult, Vec::new()),
        ":copyfrom" => (Command::Exec, Vec::new()),
        ":one" | ":many" => {
            let (fields, columns) = results::columns(query, resolver, models)?;
            let shape = match fields.as_slice() {
                [] => bail!(
                    "{cmd} has no result columns in sqlc's report; if the query returns rows, \
                     list them with casts (SELECT n::int, s::text FROM ...), otherwise use :exec"
                ),
                [RowField::Column(field)] => Shape::Scalar(field.ty.clone()),
                _ => results::row_shape(
                    models,
                    &query.columns,
                    fields,
                    resolver.catalog,
                    format!("{class}Row"),
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
    })
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
