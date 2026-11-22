use ruff_python_ast::{Stmt, StmtClassDef};
use sqlc_gen_python::api::names::{self, Binding, Repeats};
use sqlc_gen_python::config::Config;
use sqlc_gen_python::engine::Engine;
use sqlc_gen_python::plugin;

use super::python::{Shape, fields, is_enum, is_error};
use super::records::{documented, reads_in_order};

/// The models module holds one enum per catalog enum, its members its labels in order and named as the generator
/// names them, and one dataclass per table, its fields the columns in order, each optional exactly when sqlc says the
/// column admits NULL and a list exactly as deep as its array. Only sqlc's request can tell us that, as no type checker
/// or runtime probe can see a non-null column annotated `X | None`: every value of `X` fits.
pub(super) fn models_follow_catalog(
    (request, config): (&plugin::GenerateRequest, &Config),
    models: &[Stmt],
    problems: &mut Vec<String>,
) {
    let schemas = || request.catalog.iter().flat_map(|catalog| catalog.user_schemas());
    let engine = Engine::of(request).unwrap();
    let mut enums: Vec<&plugin::Enum> = schemas()
        .flat_map(|schema| schema.enums.iter().filter(move |labels| !engine.is_set(schema, labels)))
        .collect();
    // The options' `rename` applies to enum labels too.
    let rename = &config.rename;
    let records = config.records;
    let omits_unused = config.models.omit_unused;
    let mut tables: Vec<(&plugin::Table, Vec<(String, Shape)>)> = (schemas())
        .flat_map(|schema| &schema.tables)
        .map(|table| {
            let spellings: Vec<&str> = table.columns.iter().map(|c| c.name.as_str()).collect();
            let names = names::names(&spellings, Repeats::Forbidden, Binding::Field, records).unwrap_or_default();
            (
                table,
                names.into_iter().zip(table.columns.iter().map(Shape::from)).collect(),
            )
        })
        .collect();

    for class in models.iter().filter_map(Stmt::as_class_def_stmt) {
        let what = format!("models.py: class `{}`", class.name);
        if is_error(class) {
            continue;
        }
        if is_enum(class) {
            let (names, values): (Vec<&str>, Vec<String>) = members(class).into_iter().unzip();
            let Some(position) = enums.iter().position(|e| e.vals == values) else {
                problems.push(format!("{what} has no SQL enum of its values {values:?}"));
                continue;
            };
            let sql_enum = enums.swap_remove(position);
            let spelling = |label| rename.get(label).map_or(label, String::as_str);
            let expected = names::members(&sql_enum.vals, spelling).unwrap_or_default();
            if !names.iter().eq(expected.iter().map(|member| &member.name)) {
                problems.push(format!(
                    "{what}: members {names:?} are not named after their labels {values:?}"
                ));
            }
            documented(&what, class, &sql_enum.comment, std::iter::empty(), problems);
            continue;
        }

        let fields = fields(class);
        reads_in_order(class, &vec![None; fields.len()], problems);
        let found: Vec<(String, Shape)> = (fields.iter())
            .map(|(name, annotation)| (name.to_string(), Shape::of(annotation)))
            .collect();
        match tables.iter().position(|(_, columns)| *columns == found) {
            Some(position) => {
                let (table, _) = tables.swap_remove(position);
                let comments = table.columns.iter().map(|c| c.comment.as_str());
                documented(&what, class, &table.comment, comments, problems);
            }
            None => problems.push(format!(
                "{what} has no table with its fields' names, nullability and array depths"
            )),
        }
    }

    // Such an entry writes the classes its queries use alone, which `src/api/models_spec.rs` specifies
    if omits_unused {
        return;
    }
    for (table, _) in tables {
        let name = table.rel.as_ref().map_or("", |rel| rel.name.as_str());
        problems.push(format!(
            "models.py: table `{name}` has no class with its columns' names, nullability and array depths"
        ));
    }
    for sql_enum in enums {
        problems.push(format!("models.py: enum `{}` has no class", sql_enum.name));
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
