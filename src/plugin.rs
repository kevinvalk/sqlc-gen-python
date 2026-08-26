//! The sqlc plugin wire protocol.
//!
//! The bulk of this module is generated from `protos/sqlc.proto` (a verbatim
//! copy of sqlc's own `codegen.proto`). On top of that it adds the handful of
//! helpers sqlc keeps in its Go SDK and that every codegen backend needs.

#[cfg(test)]
#[path = "plugin_spec.rs"]
mod spec;

include!(concat!(env!("OUT_DIR"), "/plugin.rs"));

/// Schemas sqlc always reports but that no generated model should describe.
const INTERNAL_SCHEMAS: [&str; 2] = ["pg_catalog", "information_schema"];

impl Identifier {
    /// The `schema.name` form sqlc's type tables are keyed by.
    pub fn data_type(&self) -> String {
        if self.schema.is_empty() {
            self.name.clone()
        } else {
            format!("{}.{}", self.schema, self.name)
        }
    }

    /// Whether this identifier names `other`.
    ///
    /// A column's table reference often omits the schema, in which case it means
    /// the catalog's default schema rather than "any schema".
    pub fn is(&self, other: &Identifier, default_schema: &str) -> bool {
        let schema = if self.schema.is_empty() {
            default_schema
        } else {
            &self.schema
        };
        self.catalog == other.catalog && schema == other.schema && self.name == other.name
    }
}

impl Schema {
    /// Whether this schema is one of the engine's own, which we never model.
    pub fn is_internal(&self) -> bool {
        INTERNAL_SCHEMAS.contains(&self.name.as_str())
    }

    /// The prefix that disambiguates names in this schema from the default one.
    ///
    /// Objects in the default schema keep their bare name; everything else is
    /// qualified, so `analytics.event` becomes `AnalyticsEvent`.
    pub fn qualify(&self, name: &str, default_schema: &str) -> String {
        if self.name == default_schema {
            name.to_string()
        } else {
            format!("{}_{}", self.name, name)
        }
    }
}

impl Column {
    /// The `schema.name` the engine type tables are keyed by.
    pub fn data_type(&self) -> String {
        self.r#type
            .as_ref()
            .map(Identifier::data_type)
            .unwrap_or_default()
    }
}

impl Catalog {
    /// The schemas worth generating code for, in catalog order.
    pub fn user_schemas(&self) -> impl Iterator<Item = &Schema> {
        self.schemas.iter().filter(|s| !s.is_internal())
    }
}
