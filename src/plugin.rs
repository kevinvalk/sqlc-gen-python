//! Generated from `protos/sqlc.proto`, a verbatim copy of sqlc's own `codegen.proto`.

#[cfg(test)]
#[path = "plugin_spec.rs"]
mod spec;

include!(concat!(env!("OUT_DIR"), "/plugin.rs"));

impl Identifier {
    /// An empty schema means the catalog's default schema, so we need the catalog to compare.
    pub fn is(&self, other: &Identifier, catalog: &Catalog) -> bool {
        self.catalog == other.catalog
            && self.name == other.name
            && catalog
                .schema(&self.schema)
                .is_some_and(|schema| schema.name == other.schema)
    }
}

impl Column {
    /// `schema.name`, as each engine's type map reads it
    pub fn data_type(&self) -> String {
        match &self.r#type {
            Some(ty) if !ty.schema.is_empty() => format!("{}.{}", ty.schema, ty.name),
            Some(ty) => ty.name.clone(),
            None => String::new(),
        }
    }
}

impl Catalog {
    pub(crate) fn table(&self, schema: &str, name: &str) -> Option<&Table> {
        self.schema(schema)?
            .tables
            .iter()
            .find(|t| t.rel.as_ref().is_some_and(|r| r.name == name))
    }

    /// All but the engine's own schemas, which sqlc may report but no model describes.
    pub fn user_schemas(&self) -> impl Iterator<Item = &Schema> {
        (self.schemas.iter()).filter(|schema| !["pg_catalog", "information_schema"].contains(&schema.name.as_str()))
    }

    /// sqlc often leaves the schema out, which means the default schema and not "any schema".
    pub fn schema(&self, name: &str) -> Option<&Schema> {
        let name = if name.is_empty() { &self.default_schema } else { name };
        self.user_schemas().find(|schema| schema.name == name)
    }
}
