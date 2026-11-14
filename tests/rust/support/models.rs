//! The invariants holding the models module to sqlc's catalog: one class per
//! enum and table, named, shaped and documented as the catalog says.

use ruff_python_ast::{Stmt, StmtClassDef};
use sqlc_gen_python::api::names::{self, Repeats};
use sqlc_gen_python::plugin;

use super::python::{Shape, fields, is_enum};
use super::records::{documented, reads_in_order};

/// Checks that the models module has one class per catalog enum and table.
///
/// An enum's members are its labels, in order, each named as the generator
/// names it. A table's dataclass has its columns' names, in order, each
/// annotated optional exactly when sqlc says the column admits NULL, and as
/// a list exactly as deep as its array: no type checker or runtime probe can
/// see a non-null column annotated `X | None`, because every value of `X`
/// fits; only sqlc's request can say. Each class and field is documented by
/// its own comment.
pub(super) fn models_follow_catalog(
    request: &plugin::GenerateRequest,
    models: &[Stmt],
    problems: &mut Vec<String>,
) {
    let schemas = || {
        request
            .catalog
            .iter()
            .flat_map(|catalog| catalog.user_schemas())
    };
    let mut enums: Vec<&plugin::Enum> = schemas().flat_map(|schema| &schema.enums).collect();
    // Each table, and its columns' names and shapes.
    let mut tables: Vec<(&plugin::Table, Vec<(String, Shape)>)> = (schemas())
        .flat_map(|schema| &schema.tables)
        .map(|table| {
            let spellings: Vec<&str> = table.columns.iter().map(|c| c.name.as_str()).collect();
            let names = names::names(&spellings, Repeats::Forbidden).unwrap_or_default();
            (
                table,
                names
                    .into_iter()
                    .zip(table.columns.iter().map(Shape::from))
                    .collect(),
            )
        })
        .collect();

    for class in models.iter().filter_map(Stmt::as_class_def_stmt) {
        let what = format!("models.py: class `{}`", class.name);
        if is_enum(class) {
            let (names, values): (Vec<&str>, Vec<String>) = members(class).into_iter().unzip();
            let Some(sql) = enums.iter().position(|e| e.vals == values) else {
                problems.push(format!("{what} has no SQL enum of its values {values:?}"));
                continue;
            };
            let sql = enums.swap_remove(sql);
            let expected = names::members(&sql.vals).unwrap_or_default();
            if !names.iter().eq(expected.iter().map(|member| &member.name)) {
                problems.push(format!(
                    "{what}: members {names:?} are not named after their labels {values:?}"
                ));
            }
            documented(&what, class, &sql.comment, std::iter::empty(), problems);
            continue;
        }
        let fields = fields(class);
        reads_in_order(class, &vec![None; fields.len()], problems);
        let found: Vec<(String, Shape)> = (fields.iter())
            .map(|(name, annotation)| (name.to_string(), Shape::of(annotation)))
            .collect();
        match tables.iter().position(|(_, columns)| *columns == found) {
            Some(table) => {
                let (table, _) = tables.swap_remove(table);
                let comments = table.columns.iter().map(|c| c.comment.as_str());
                documented(&what, class, &table.comment, comments, problems);
            }
            None => problems.push(format!(
                "{what} has no table with its fields' names, nullability and array depths"
            )),
        }
    }
    for (table, _) in tables {
        let name = table.rel.as_ref().map_or("", |rel| rel.name.as_str());
        problems.push(format!(
            "models.py: table `{name}` has no class with its columns' names, nullability and array depths"
        ));
    }
    for sql in enums {
        problems.push(format!("models.py: enum `{}` has no class", sql.name));
    }
}

/// An enum class's members, as their names and values.
fn members(class: &StmtClassDef) -> Vec<(&str, String)> {
    (class.body.iter().filter_map(Stmt::as_assign_stmt))
        .filter_map(|assign| {
            let [target] = &assign.targets[..] else {
                return None;
            };
            let value = assign.value.as_string_literal_expr()?.value.to_str().into();
            Some((target.as_name_expr()?.id.as_str(), value))
        })
        .collect()
}
