//! What the user asked for.
//!
//! These are the keys users write under `options:` in a sqlc `codegen` entry.
//! They are grouped by what they affect rather than flattened into a single
//! `emit_*` namespace, and unknown keys are rejected so a typo in `sqlc.yaml`
//! fails loudly instead of silently doing nothing.
//!
//! ```yaml
//! codegen:
//!   - out: src/authors
//!     plugin: python
//!     options:
//!       package: authors
//!       backend: sqlalchemy
//!       async: true
//!       models:
//!         style: dataclass
//!       queries:
//!         style: class
//!         many: iterator
//! ```

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::backend::Backend;
use crate::plugin;

#[cfg(test)]
#[path = "config_spec.rs"]
mod spec;

/// Everything the generator needs beyond the schema itself.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// Python package the generated modules live in.
    ///
    /// Query modules import their models from here, so it must be the package
    /// the output directory is importable as. Defaults to the last segment of
    /// the codegen entry's `out`.
    pub package: String,

    /// The database access layer to generate against.
    pub backend: Backend,

    /// Emit `async def` coroutines instead of blocking functions.
    #[serde(rename = "async")]
    pub asynchronous: bool,

    pub models: ModelConfig,
    pub queries: QueryConfig,
    pub enums: EnumConfig,

    /// Leave the sqlc version out of the generated header, so output does not
    /// churn when sqlc is upgraded.
    pub omit_sqlc_version: bool,
}

/// How the shared model classes are generated.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ModelConfig {
    /// Whether to write the models module at all. Turn this off when models are
    /// shared from another package.
    pub emit: bool,

    /// Filename for the models module, relative to the codegen entry's `out`.
    pub file: String,

    /// Representation for generated classes.
    pub style: ClassStyle,

    /// Name model classes after the table verbatim, skipping singularisation.
    pub exact_table_names: bool,

    /// Table names that must not be singularised.
    pub inflection_exclude: Vec<String>,
}

/// How the per-query-file modules are generated.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct QueryConfig {
    /// Whether to write query modules at all.
    pub emit: bool,

    /// Where the query functions live.
    pub style: QuerierStyle,

    /// How `:many` results reach the caller.
    pub many: ManyStyle,

    /// How many parameters a query may take before they are bundled into a
    /// generated params class instead of being passed individually.
    ///
    /// Zero always bundles.
    pub parameter_limit: u32,

    /// Representation for generated params and row classes.
    pub style_classes: ClassStyle,
}

/// How SQL enums are generated.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct EnumConfig {
    pub style: EnumStyle,
}

/// The Python construct used for generated data classes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClassStyle {
    /// `@dataclasses.dataclass()`.
    #[default]
    Dataclass,
    /// `pydantic.BaseModel`.
    Pydantic,
}

/// Where query functions are defined.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuerierStyle {
    /// Methods on a `Querier` class holding the connection.
    #[default]
    Class,
    /// Module-level functions taking the connection as their first argument.
    Module,
}

/// How a `:many` query returns its rows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManyStyle {
    /// Stream rows, yielding as they arrive.
    #[default]
    Iterator,
    /// Buffer every row and return a list.
    List,
}

/// The base class generated enums derive from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnumStyle {
    /// `(str, enum.Enum)`, which every supported Python version accepts.
    #[default]
    StrMixin,
    /// `enum.StrEnum`, which needs Python 3.11 or newer.
    StrEnum,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            emit: true,
            file: "models.py".into(),
            style: ClassStyle::default(),
            exact_table_names: false,
            inflection_exclude: Vec::new(),
        }
    }
}

impl Default for QueryConfig {
    fn default() -> Self {
        Self {
            emit: true,
            style: QuerierStyle::default(),
            many: ManyStyle::default(),
            style_classes: ClassStyle::default(),
            parameter_limit: 4,
        }
    }
}

impl Config {
    /// Reads the options a codegen entry configured, applying defaults that can
    /// only be known from the request itself.
    pub fn from_request(request: &plugin::GenerateRequest) -> Result<Self> {
        let mut options = if request.plugin_options.is_empty() {
            Self::default()
        } else {
            serde_json::from_slice(&request.plugin_options).context("reading plugin options")?
        };

        if options.package.is_empty() {
            let out = request
                .settings
                .as_ref()
                .and_then(|s| s.codegen.as_ref())
                .map_or("", |c| c.out.as_str());
            options.package = out
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .to_string();
        }

        options.validate()?;
        Ok(options)
    }

    fn validate(&self) -> Result<()> {
        if self.package.is_empty() {
            bail!("`package` is required when the codegen entry has no `out` to infer it from");
        }
        if !self.models.file.ends_with(".py") {
            bail!(
                "`models.file` must name a Python module, got `{}`",
                self.models.file
            );
        }
        if !self.models.emit && !self.queries.emit {
            bail!("`models.emit` and `queries.emit` are both false, so nothing would be generated");
        }
        Ok(())
    }

    /// The module query files import their models from.
    pub fn models_module(&self) -> String {
        self.models
            .file
            .trim_end_matches(".py")
            .rsplit('/')
            .next()
            .unwrap_or("models")
            .to_string()
    }
}

impl ModelConfig {
    /// Whether `table` should keep its name verbatim rather than be singularised.
    pub fn is_inflection_excluded(&self, table: &str) -> bool {
        self.exact_table_names
            || self
                .inflection_exclude
                .iter()
                .any(|excluded| excluded.eq_ignore_ascii_case(table))
    }
}
