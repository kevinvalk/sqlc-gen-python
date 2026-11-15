use std::fmt::Debug;

use anyhow::Result;

use super::Api;
use super::queries::{Command, Module, Query};
use crate::backend::Backend;
use crate::config::{Config, ModelConfig};
use crate::plugin;

pub fn column(name: &str, data_type: &str, not_null: bool) -> plugin::Column {
    plugin::Column {
        name: name.into(),
        not_null,
        r#type: Some(plugin::Identifier {
            name: data_type.into(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

pub fn sql_schema(name: &str, tables: Vec<plugin::Table>, enums: Vec<plugin::Enum>) -> plugin::Schema {
    plugin::Schema {
        name: name.into(),
        tables,
        enums,
        ..plugin::Schema::default()
    }
}

pub fn table(name: &str, columns: Vec<plugin::Column>) -> plugin::Table {
    plugin::Table {
        rel: Some(plugin::Identifier {
            name: name.into(),
            ..plugin::Identifier::default()
        }),
        columns,
        ..plugin::Table::default()
    }
}

pub fn public(tables: Vec<plugin::Table>) -> Vec<plugin::Schema> {
    vec![sql_schema("public", tables, vec![])]
}

pub fn model_names(api: &Api) -> Vec<&str> {
    api.models.iter().map(|m| m.name.as_str()).collect()
}

pub fn error_of<T: Debug>(result: Result<T>) -> String {
    format!("{:#}", result.unwrap_err())
}

pub fn request(engine: &str, schemas: Vec<plugin::Schema>, queries: Vec<plugin::Query>) -> plugin::GenerateRequest {
    plugin::GenerateRequest {
        settings: Some(plugin::Settings {
            engine: engine.into(),
            ..Default::default()
        }),
        catalog: Some(plugin::Catalog {
            default_schema: "public".into(),
            schemas,
            ..Default::default()
        }),
        queries,
        ..Default::default()
    }
}

pub mod catalog {
    use super::*;

    pub fn resolve(schemas: Vec<plugin::Schema>, config: &ModelConfig) -> Api {
        resolve_with(schemas, config).unwrap()
    }

    pub fn try_resolve(schemas: Vec<plugin::Schema>) -> Result<Api> {
        resolve_with(schemas, &ModelConfig::default())
    }

    pub fn resolve_with(schemas: Vec<plugin::Schema>, config: &ModelConfig) -> Result<Api> {
        let config = Config {
            models: config.clone(),
            ..Config::of(Backend::SqlAlchemy)
        };
        resolve_options(schemas, &config)
    }

    pub fn resolve_options(schemas: Vec<plugin::Schema>, config: &Config) -> Result<Api> {
        crate::api::resolve(&request("postgresql", schemas, vec![]), config)
    }
}

pub mod authors {
    use super::*;

    pub fn of_authors(mut column: plugin::Column) -> plugin::Column {
        column.table = Some(plugin::Identifier {
            name: "authors".into(),
            ..Default::default()
        });
        column
    }

    /// The columns of `authors`: `id bigint NOT NULL, name text NOT NULL, bio text`.
    pub fn authors() -> Vec<plugin::Column> {
        vec![
            column("id", "pg_catalog.int8", true),
            column("name", "text", true),
            column("bio", "text", false),
        ]
    }

    pub fn param(number: i32, column: plugin::Column) -> plugin::Parameter {
        plugin::Parameter {
            number,
            column: Some(column),
        }
    }

    /// A parameter the user named.
    pub fn named(number: i32, name: &str, data_type: &str, not_null: bool) -> plugin::Parameter {
        param(
            number,
            plugin::Column {
                is_named_param: true,
                ..column(name, data_type, not_null)
            },
        )
    }

    pub fn query(cmd: &str, text: &str) -> plugin::Query {
        plugin::Query {
            name: "GetAuthor".into(),
            filename: "authors.sql".into(),
            cmd: cmd.into(),
            text: text.into(),
            ..Default::default()
        }
    }

    pub fn try_resolve_all(queries: Vec<plugin::Query>) -> Result<Vec<Module>> {
        try_resolve_on("postgresql", queries)
    }

    pub fn try_resolve_on(engine: &str, queries: Vec<plugin::Query>) -> Result<Vec<Module>> {
        try_resolve_with(engine, queries, &Config::of(Backend::SqlAlchemy))
    }

    pub fn try_resolve_with(engine: &str, queries: Vec<plugin::Query>, config: &Config) -> Result<Vec<Module>> {
        // The catalog's types are PostgreSQL's, so only PostgreSQL gets one
        let schemas = (engine == "postgresql")
            .then(|| {
                let mood = plugin::Enum {
                    name: "mood".into(),
                    vals: vec!["happy".into()],
                    ..Default::default()
                };
                sql_schema("public", vec![table("authors", authors())], vec![mood])
            })
            .into_iter()
            .collect();
        crate::api::resolve(&request(engine, schemas, queries), config).map(|api| api.modules)
    }

    pub fn try_resolve(query: plugin::Query) -> Result<Query> {
        try_resolve_all(vec![query]).map(|mut modules| modules.remove(0).queries.remove(0))
    }

    pub fn resolve(query: plugin::Query) -> Query {
        try_resolve(query).unwrap()
    }

    pub fn on(engine: &str, query: plugin::Query) -> Result<Query> {
        try_resolve_on(engine, vec![query]).map(|mut modules| modules.remove(0).queries.remove(0))
    }

    pub fn error(query: plugin::Query) -> String {
        error_of(try_resolve(query))
    }

    /// A statement with an item per column, as one whose list we cannot read leaves every column optional.
    pub fn selecting(columns: &[plugin::Column]) -> String {
        let list: Vec<&str> = columns.iter().map(|_| "x").collect();
        format!("SELECT {} FROM authors", list.join(", "))
    }

    pub fn returning(cmd: &str, columns: Vec<plugin::Column>) -> Command {
        resolve(plugin::Query {
            text: selecting(&columns),
            columns,
            ..query(cmd, "")
        })
        .command
    }

    pub fn params(params: Vec<plugin::Parameter>, text: &str) -> Query {
        resolve(plugin::Query {
            params,
            ..query(":exec", text)
        })
    }
}
