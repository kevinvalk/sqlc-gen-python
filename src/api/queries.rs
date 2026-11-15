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

#[derive(Debug, Clone, PartialEq)]
pub struct Module {
    /// The query file's stem, so `invoices.sql` is the module `invoices`.
    pub name: String,
    pub queries: Vec<Query>,
}

/// One query, as a SQL constant, perhaps a class, and a function (`docs/python-output.md` §6).
#[derive(Debug, Clone, PartialEq)]
pub struct Query {
    pub function: String,
    pub constant: String,
    pub comment: String,
    /// The statement as sqlc wrote it, cut at its placeholders.
    pub sql: Sql,
    /// The function's parameters, each name sqlc reported once.
    pub params: Vec<Field>,
    /// The table (`schema` empty for the search path's) and its columns, one per parameter, that a `:copyfrom` writes
    /// into. None when the statement binds a parameter twice, as `COPY` would then write one column for the two it
    /// inserts.
    pub copy_into: Option<CopyInto>,
    /// Every column of the result as the database returns it, embeds expanded.
    pub columns: Vec<Field>,
    pub command: Command,
    pub takes: Takes,
    /// Whether a `:one` raises without a row instead of returning `None`. The options decide, except for a statement
    /// that always returns a row (`SELECT count(*) ...`): it never lacks one, so it returns its row's type.
    pub raises: bool,
}

impl Query {
    /// The `<Query>Row` class the query defines, if it does, and its fields.
    pub fn row(&self) -> Option<(&str, &[RowField])> {
        match &self.command {
            Command::One(Shape::Row(class, fields)) | Command::Many(Shape::Row(class, fields)) => Some((class, fields)),
            _ => None,
        }
    }

    /// The classes the query defines: its `Params`, then its `Row`.
    pub fn classes(&self) -> impl Iterator<Item = &str> {
        (self.takes.class().into_iter()).chain(self.row().map(|(class, _)| class))
    }
}

/// How a query's function takes its parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Takes {
    /// Each as a keyword.
    Keywords,
    /// All as one record of the `<Query>Params` class, `params`, when there are more than the options' `max_params`.
    Record(String),
    /// `rows` of the `<Query>Params` class, running its command once per row: a `:copyfrom`, or a batch (`:batchexec`
    /// is an `:exec` per row, `:batchone` a `:one`, `:batchmany` a `:many`).
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
    /// Returns the only row, or `None` without one unless the query [`raises`](Query::raises); a second row raises.
    One(Shape),
    Many(Shape),
}

/// What one row of a result becomes.
#[derive(Debug, Clone, PartialEq)]
pub enum Shape {
    /// The value of the only column.
    Scalar(FieldType),
    /// The model of the table whose columns these are, exactly.
    Model(String),
    /// The `<Query>Row` class named here, of these fields.
    Row(String, Vec<RowField>),
}

impl Shape {
    /// The annotation of a row of this shape in its query module, optional when the row may be `missing`.
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

    /// What a `row` the database returns becomes: its only column, or its record.
    pub fn read(&self, row: impl Into<ast::Expr>) -> ast::Expr {
        match self {
            Self::Scalar(ty) => ty.read(row.index(int(0)), Scope::Queries),
            record => record.annotation(false).attr(FROM_ROW).call([row.into()]),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum RowField {
    Column(Field),
    /// `sqlc.embed(t)`, the model of `t` built from the next `width` columns. When an outer join may leave `t` without
    /// a row, `missing` is the model's first NOT NULL column, which is NULL only then, and the field is then `None`.
    Embed {
        name: String,
        model: String,
        width: usize,
        missing: Option<usize>,
    },
}

impl RowField {
    pub fn name(&self) -> &str {
        match self {
            Self::Column(field) => &field.name,
            Self::Embed { name, .. } => name,
        }
    }
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
    let mut defined = HashMap::new(); // Each module's functions and classes, with the query each is for
    let mut misnamed = HashSet::new();
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
        for python_name in std::iter::once(resolved.function.as_str()).chain(resolved.classes()) {
            if let Some(first) = defined.insert((name.clone(), python_name.to_owned()), &query.name) {
                problems.note(anyhow::anyhow!(
                    "{file}: \"{first}\" and \"{}\" both become the Python name \"{python_name}\"",
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

/// The module a query file becomes: its stem, with each `-` a `_`. That has to be a module name, and not one of the
/// modules the plugin writes itself.
fn module_name(file: &str) -> Result<String> {
    let stem = (Path::new(file).file_stem())
        .and_then(OsStr::to_str)
        .unwrap_or("")
        .replace('-', "_");
    if ["models", "__init__"].contains(&stem.to_lowercase().as_str()) {
        bail!("{file}: would overwrite the generated module \"{stem}.py\"; rename the file");
    }
    if !python::is_module_name(&stem) {
        bail!("{file}: \"{stem}\" is not a lowercase Python module name; rename the file");
    }
    Ok(stem)
}

fn resolve(query: &plugin::Query, resolver: &Resolver, models: &[Model], config: &Config) -> Result<Query> {
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

    // PostgreSQL's `$N` is its parameter's number. We number MySQL's and SQLite's `?` by sqlc's order of the
    // parameters, as sqlc-gen-go binds them, because the numbers sqlc reports there may not be the text's.
    let binds: Vec<Bind> = (query.params.iter().zip(&uses).enumerate())
        .map(|(i, (parameter, field))| {
            let column = parameter.column.as_ref();
            Bind {
                number: match resolver.engine {
                    Engine::PostgreSql => parameter.number,
                    Engine::MySql | Engine::Sqlite => i as i32 + 1,
                },
                name: &field.name,
                slice: (column.filter(|c| c.is_sqlc_slice)).map(|c| c.name.as_str()),
            }
        })
        .collect();
    let sql = resolver.engine.sql(&query.text, &binds)?;

    // A batch runs one statement for every row, while a slice makes it another statement for each number of items
    if batch && (sql.0.iter()).any(|piece| matches!(piece, Piece::Slice { .. })) {
        bail!(
            "{} runs one statement for every row, and a sqlc.slice makes it another for each \
             number of items; use :exec, once per row",
            query.cmd
        )
    }

    let placeholders = (sql.0.iter()).filter(|piece| !matches!(piece, Piece::Text(_))).count();
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

    let takes = match batch {
        true => Takes::Rows(params_class),
        false if config.max_params.is_some_and(|max| params.len() > max) => Takes::Record(params_class),
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
                _ => results::row_shape(models, &query.columns, fields, resolver.catalog, format!("{class}Row")),
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

/// The table sqlc reports a `:copyfrom` inserts into, and each parameter's column of it.
fn copy_into(query: &plugin::Query) -> Option<CopyInto> {
    let table = query.insert_into_table.as_ref().filter(|_| query.cmd == ":copyfrom")?;
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
