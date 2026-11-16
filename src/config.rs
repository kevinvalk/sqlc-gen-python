use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::backend::Backend;
use crate::plugin;
use crate::python::{self, Annotation};

#[cfg(test)]
#[path = "config_spec.rs"]
mod spec;

/// The keys under `options:` of a sqlc `codegen` entry. Keep in sync with "Options" in the README.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)] // sqlc only shows our stderr on failure, so a typo in `sqlc.yaml` must be an error
pub struct Config {
    /// Required, as no one library suits every engine and application.
    pub backend: Backend,

    /// Required, as either default is a surprise to whoever expected the other.
    #[serde(rename = "async")]
    pub asynchronous: bool,

    #[serde(default)]
    pub models: ModelConfig,

    /// Leaves the sqlc version out of the generated header, so output does not churn when sqlc is upgraded.
    #[serde(default)]
    pub omit_sqlc_version: bool,

    #[serde(default)]
    pub overrides: Vec<Override>,

    /// Python names for SQL names: a table's or enum's class, a column's field or parameter.
    #[serde(default)]
    pub rename: BTreeMap<String, String>,

    /// Whether a `:one` without a row raises (SQLAlchemy's `NoResultFound`, or `models.NoRowError`) rather
    /// than return `None`. A second row raises either way, as a lookup that is not unique is a bug and not an absence.
    #[serde(default)]
    pub raise_on_none: bool,

    /// A function with more parameters takes them as one `<Query>Params` record instead of as keywords.
    pub max_params: Option<usize>,

    /// What a type we cannot verify is annotated, which is otherwise an error.
    pub unknown_type: Option<UnknownType>,
}

/// The one annotation for every type we cannot verify, which says nothing that may be false. `object` makes the user
/// narrow a value before using it, `Any` lets them use it as anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum UnknownType {
    Any,
    #[serde(rename = "object")]
    Object,
}

impl UnknownType {
    pub fn annotation(self) -> ruff_python_ast::Expr {
        match self {
            Self::Any => python::vocabulary::ANY.into(),
            Self::Object => python::vocabulary::OBJECT.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Override {
    /// A type as sqlc reports it, with `pg_catalog.` optional (`int4`).
    pub db_type: Option<String>,
    /// `table.column` or `schema.table.column`
    pub column: Option<String>,
    pub python_type: Annotation,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ModelConfig {
    /// The module the query modules read their models from (`gateway.models`), which another `codegen` entry of the
    /// same schema and backend writes. This one then writes its query modules alone.
    pub module: Option<String>,

    pub exact_table_names: bool,

    pub inflection_exclude: Vec<String>,
}

impl Config {
    pub fn from_request(request: &plugin::GenerateRequest) -> Result<Self> {
        let options: &[u8] = match request.plugin_options.as_slice() {
            [] => b"{}", // sqlc sends empty bytes for a codegen entry without `options:`
            options => options,
        };
        let config: Self = serde_json::from_slice(options).context("reading plugin options")?;

        // Query modules import the package's own models relatively, so it is imported by the name of its directory,
        // which then has to be a package name
        let codegen = request.settings.as_ref().and_then(|settings| settings.codegen.as_ref());
        let out = codegen.map_or("", |codegen| codegen.out.as_str());
        if config.models.module.is_none()
            && let Some(package) = Path::new(out).file_name().and_then(|name| name.to_str())
            && !python::is_module_name(package)
        {
            bail!(
                "out \"{out}\": \"{package}\" is not a lowercase Python package name, and the \
                 generated package is imported by it; rename the directory"
            );
        }
        if let Some(module) = &config.models.module
            && !module.split('.').all(python::is_module_name)
        {
            bail!("models.module \"{module}\" is no dotted name of a lowercase Python module (`app.models`)");
        }

        for (i, rule) in config.overrides.iter().enumerate() {
            if rule.db_type.is_some() == rule.column.is_some() {
                bail!("overrides[{i}]: name exactly one of db_type and column");
            }
        }
        if let Some((sql, python)) = (config.rename.iter()).find(|(_, python)| !python::is_public_name(python)) {
            bail!("rename: \"{sql}\" to \"{python}\", which is no public Python name");
        }
        Ok(config)
    }

    /// The modules the overrides' types read from, which generated code has to import.
    pub fn modules(&self) -> Vec<String> {
        (self.overrides.iter())
            .flat_map(|rule| rule.python_type.modules.iter().cloned())
            .collect()
    }
}

impl ModelConfig {
    pub fn is_inflection_excluded(&self, table: &str) -> bool {
        self.exact_table_names
            || self
                .inflection_exclude
                .iter()
                .any(|excluded| excluded.eq_ignore_ascii_case(table))
    }
}

#[cfg(test)]
impl Config {
    pub fn of(backend: Backend) -> Self {
        Self {
            backend,
            asynchronous: false,
            models: ModelConfig::default(),
            omit_sqlc_version: false,
            overrides: Vec::new(),
            rename: BTreeMap::new(),
            raise_on_none: false,
            max_params: None,
            unknown_type: None,
        }
    }
}
