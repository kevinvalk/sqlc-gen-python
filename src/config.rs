//! What the user asked for: the keys under `options:` in a sqlc `codegen`
//! entry, as specified in `docs/python-output.md` §7.
//!
//! Unknown keys are rejected, because sqlc shows a plugin's stderr only when it
//! fails: a typo in `sqlc.yaml` must be an error, not a silent no-op.
//!
//! ```yaml
//! codegen:
//!   - out: src/invoicing
//!     plugin: python
//!     options:
//!       backend: psycopg
//!       async: false
//!       models:
//!         inflection_exclude: [campus]
//! ```

use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::backend::Backend;
use crate::{plugin, python};

#[cfg(test)]
#[path = "config_spec.rs"]
mod spec;

/// Everything the generator needs beyond the request itself.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The library the generated code runs its SQL through. Required: no one
    /// library suits every engine and application.
    pub backend: Backend,

    /// Generate coroutines (`async def`) rather than blocking functions.
    /// Required: either is a surprise to whoever expected the other.
    #[serde(rename = "async")]
    pub asynchronous: bool,

    #[serde(default)]
    pub models: ModelConfig,

    /// Leave the sqlc version out of the generated header, so output does not
    /// churn when sqlc is upgraded.
    #[serde(default)]
    pub omit_sqlc_version: bool,
}

#[cfg(test)]
impl Config {
    /// The options naming only `backend`.
    pub fn of(backend: Backend) -> Self {
        Self {
            backend,
            asynchronous: false,
            models: ModelConfig::default(),
            omit_sqlc_version: false,
        }
    }
}

/// How model classes are named.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ModelConfig {
    /// Name model classes after the table verbatim, skipping singularisation.
    pub exact_table_names: bool,

    /// Table names that must not be singularised.
    pub inflection_exclude: Vec<String>,
}

impl Config {
    /// Reads the options a codegen entry configured. sqlc sends empty bytes
    /// when there are none.
    ///
    /// The generated package imports its models relatively, so it is imported
    /// by the name of its directory, `out`'s last part, which must be one.
    pub fn from_request(request: &plugin::GenerateRequest) -> Result<Self> {
        let codegen = request.settings.as_ref().and_then(|s| s.codegen.as_ref());
        let out = codegen.map_or("", |codegen| codegen.out.as_str());
        if let Some(package) = Path::new(out).file_name().and_then(|name| name.to_str())
            && !python::is_module_name(package)
        {
            bail!(
                "out \"{out}\": \"{package}\" is not a lowercase Python package name, and the \
                 generated package is imported by it; rename the directory"
            );
        }
        let options: &[u8] = match request.plugin_options.as_slice() {
            [] => b"{}",
            options => options,
        };
        serde_json::from_slice(options).context("reading plugin options")
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
