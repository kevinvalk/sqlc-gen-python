//! The sqlc plugin wire protocol.
//!
//! The bulk of this module is generated from `protos/sqlc.proto` (a verbatim
//! copy of sqlc's own `codegen.proto`), plus a few helpers for reading the
//! catalog.

#[cfg(test)]
#[path = "plugin_spec.rs"]
mod spec;

include!(concat!(env!("OUT_DIR"), "/plugin.rs"));

impl Identifier {
    /// Whether this identifier names `other`, a table of `catalog`.
    pub fn is(&self, other: &Identifier, catalog: &Catalog) -> bool {
        self.catalog == other.catalog
            && self.name == other.name
            && catalog
                .schema(&self.schema)
                .is_some_and(|schema| schema.name == other.schema)
    }
}

impl Column {
    /// The `schema.name` of the column's type, which the type map
    /// (`engine/postgresql/types.rs`) reads.
    pub fn data_type(&self) -> String {
        match &self.r#type {
            Some(ty) if !ty.schema.is_empty() => format!("{}.{}", ty.schema, ty.name),
            Some(ty) => ty.name.clone(),
            None => String::new(),
        }
    }
}

impl Catalog {
    /// The schemas worth generating code for, in catalog order: all but the
    /// engine's own, which sqlc may report but no model describes.
    pub fn user_schemas(&self) -> impl Iterator<Item = &Schema> {
        (self.schemas.iter())
            .filter(|s| !["pg_catalog", "information_schema"].contains(&s.name.as_str()))
    }

    /// The user schema a reference names. sqlc often leaves the schema out,
    /// which means the default schema, not "any schema".
    pub fn schema(&self, name: &str) -> Option<&Schema> {
        let name = if name.is_empty() {
            &self.default_schema
        } else {
            name
        };
        self.user_schemas().find(|s| s.name == name)
    }
}
