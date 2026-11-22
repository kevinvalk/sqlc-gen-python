use ruff_python_ast as ast;

use super::{Attribute, documentation, record};
use crate::api::Api;
use crate::api::models::{Enum, Model, enum_array};
use crate::config::Records;
use crate::python::vocabulary::{CLASSMETHOD, CLS, ENUM, FROM_ARRAY, LIST, STR, TEXT};
use crate::python::{
    Expression, Function, Scope, assign, class, docstring, forward, name, parameter, receiver, return_, string,
};

/// The classes every query module shares.
pub fn module(api: &Api, records: Records) -> Vec<ast::Stmt> {
    let arrays = api.text_array_enums();
    (api.enums.iter())
        .map(|sql_enum| enum_class(sql_enum, arrays.contains(sql_enum.name.as_str())))
        .chain(api.models.iter().map(|model| model_class(model, records)))
        .collect()
}

/// An `enum.StrEnum`, so every driver binds a member as its label and `str(member)` is that label.
///
/// Where the driver returns an array of it as the array's text, the enum parses it (`from_array`). Its labels need no
/// quoting there, so the text is its labels between braces, split by commas.
fn enum_class(sql_enum: &Enum, from_array: bool) -> ast::Stmt {
    let members = (sql_enum.members.iter()).map(|member| assign(&member.name, string(&member.value)));
    let from_array = from_array.then(|| {
        let members = enum_array(TEXT.into(), CLS.into(), false);
        Function {
            decorators: vec![CLASSMETHOD.into()],
            parameters: vec![receiver(CLS), parameter(TEXT, STR)],
            body: vec![
                docstring("The members of an array of this enum, as PostgreSQL writes one: `{a,b}`."),
                return_(members),
            ],
            ..Function::new(FROM_ARRAY, forward(LIST.index(name(&sql_enum.name))))
        }
        .into()
    });

    class(
        &sql_enum.name,
        [],
        [ENUM.attr("StrEnum")],
        (documentation(&sql_enum.comment).into_iter())
            .chain(members)
            .chain(from_array),
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
