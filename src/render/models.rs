//! The models module: the classes every query module shares
//! (`docs/python-output.md` §4).

use ruff_python_ast as ast;

use super::{Attribute, documentation, record};
use crate::api::Api;
use crate::api::models::{Enum, Model};
use crate::python::vocabulary::{CLASSMETHOD, CLS, ENUM, FROM_ARRAY, LABEL, LIST, SELF, STR, TEXT};
use crate::python::{
    Expression, Function, Scope, assign, class, docstring, each, forward, int, list_of, minus,
    name, parameter, receiver, return_, string,
};

/// Each enum's class, then each table's.
pub fn module(api: &Api, text_arrays: bool) -> Vec<ast::Stmt> {
    let arrays = api.array_enums();
    (api.enums.iter())
        .map(|e| enum_class(e, text_arrays && arrays.contains(e.name.as_str())))
        .chain(
            api.models
                .iter()
                .map(|model| model_class(model, text_arrays)),
        )
        .collect()
}

/// A `str`-mixin enum: both drivers bind a member as its label, and
/// `enum.StrEnum` would need Python 3.11. Without the `__str__` override,
/// `str(member)` changes between Python 3.10 and 3.12.
///
/// Where the driver returns an array of it as the array's text, the enum
/// parses one (`from_array`): its labels need no quoting there, so the text is
/// its labels between braces, split by commas.
fn enum_class(sql_enum: &Enum, from_array: bool) -> ast::Stmt {
    let members =
        (sql_enum.members.iter()).map(|member| assign(&member.name, string(&member.value)));
    let str_method = Function {
        parameters: vec![receiver(SELF)],
        body: vec![return_(STR.attr("__str__").call([SELF.into()]))],
        ..Function::new("__str__", STR)
    };
    let from_array = from_array.then(|| {
        let labels = (TEXT.slice(int(1), minus(int(1))).attr("split")).call([string(",")]);
        let members = list_of(CLS.call([LABEL.into()]), each(LABEL, labels).when(LABEL));
        Function {
            decorators: vec![CLASSMETHOD.into()],
            parameters: vec![receiver(CLS), parameter(TEXT, STR)],
            body: vec![
                docstring(
                    "The members of an array of this enum, as PostgreSQL writes one: `{a,b}`.",
                ),
                return_(members),
            ],
            ..Function::new(FROM_ARRAY, forward(LIST.index(name(&sql_enum.name))))
        }
        .into()
    });

    class(
        &sql_enum.name,
        [],
        [STR.into(), ENUM.attr("Enum")],
        (documentation(&sql_enum.comment).into_iter())
            .chain(members)
            .chain([str_method.into()])
            .chain(from_array),
    )
}

/// A table's record, read from a row of exactly its columns.
fn model_class(model: &Model, text_arrays: bool) -> ast::Stmt {
    let attributes: Vec<Attribute> = (model.fields.iter().enumerate())
        .map(|(i, field)| Attribute {
            comment: field.comment.clone(),
            ..Attribute::column(field, i, Scope::Models, text_arrays)
        })
        .collect();
    record(&model.name, &model.comment, &attributes)
}
