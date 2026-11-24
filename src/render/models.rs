use ruff_python_ast as ast;

use super::{Attribute, documentation, record};
use crate::api::Api;
use crate::api::models::{Enum, Model};
use crate::config::Records;
use crate::python::vocabulary::ENUM;
use crate::python::{Expression, Scope, assign, class, string};

/// The classes every query module shares.
pub fn module(api: &Api, records: Records) -> Vec<ast::Stmt> {
    (api.enums.iter())
        .map(enum_class)
        .chain(api.models.iter().map(|model| model_class(model, records)))
        .collect()
}

/// An `enum.StrEnum`, so every driver binds a member as its label and `str(member)` is that label.
fn enum_class(sql_enum: &Enum) -> ast::Stmt {
    let members = (sql_enum.members.iter()).map(|member| assign(&member.name, string(&member.value)));
    class(
        &sql_enum.name,
        [],
        [ENUM.attr("StrEnum")],
        documentation(&sql_enum.comment).into_iter().chain(members),
    )
}

fn model_class(model: &Model, records: Records) -> ast::Stmt {
    let attributes: Vec<Attribute> = (model.fields.iter().enumerate())
        .map(|(index, field)| Attribute {
            comment: field.comment.clone(),
            ..Attribute::column(field, index, Scope::Models)
        })
        .collect();
    record(&model.name, &model.comment, &attributes, records)
}
