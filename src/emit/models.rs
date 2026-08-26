//! The models module: the classes every query module shares.

use anyhow::Result;
use ruff_python_ast as ast;

use super::{ClassShape, header};
use crate::config::{Config, EnumStyle};
use crate::plugin;
use crate::python::{self, Import, Imports, Scope};
use crate::schema::{Enum, Model, Schema};

/// Renders the models module.
pub fn module(
    schema: &Schema,
    config: &Config,
    request: &plugin::GenerateRequest,
) -> Result<plugin::File> {
    let style = ClassShape::of(config.models.style);

    let classes = (schema.enums.iter())
        .map(|sql_enum| enum_class(sql_enum, config.enums.style))
        .chain(schema.models.iter().map(|model| model_class(model, &style)));

    let module = python::module(
        imports(schema, &style)
            .statements()
            .into_iter()
            .chain(classes),
    );

    Ok(plugin::File {
        name: config.models.file.clone(),
        contents: format!("{}{}", header(request, config), python::render(&module)?).into_bytes(),
    })
}

/// Everything the models module imports, read off what it is about to write.
fn imports(schema: &Schema, style: &ClassShape) -> Imports {
    let fields = (schema.models.iter())
        .flat_map(|model| &model.fields)
        .flat_map(|field| field.ty.imports());

    let enums = (!schema.enums.is_empty()).then(|| Import::module("enum"));
    let models = (!schema.models.is_empty()).then(|| style.import.clone());

    fields.chain(enums).chain(models).collect()
}

fn enum_class(sql_enum: &Enum, style: EnumStyle) -> ast::Stmt {
    // `enum.StrEnum` already mixes in `str`; the older spelling has to say so.
    let bases = match style {
        EnumStyle::StrEnum => vec![python::expr("enum.StrEnum")],
        EnumStyle::StrMixin => vec![python::expr("str"), python::expr("enum.Enum")],
    };
    let members = (sql_enum.members.iter())
        .map(|member| python::assign(&member.name, python::string(&member.value)));

    python::class(
        &sql_enum.name,
        [],
        bases,
        documented(&sql_enum.comment, members),
    )
}

fn model_class(model: &Model, style: &ClassShape) -> ast::Stmt {
    let fields = (model.fields.iter())
        .map(|field| python::ann_assign(&field.name, field.ty.annotation(Scope::Models), None));

    python::class(
        &model.name,
        style.decorator.clone(),
        style.base.clone(),
        documented(&model.comment, fields),
    )
}

/// A class body, preceded by a docstring when the schema documented the object.
fn documented(
    comment: &str,
    body: impl IntoIterator<Item = ast::Stmt>,
) -> impl Iterator<Item = ast::Stmt> {
    let docstring = (!comment.trim().is_empty()).then(|| python::docstring(comment.trim()));
    docstring.into_iter().chain(body)
}
