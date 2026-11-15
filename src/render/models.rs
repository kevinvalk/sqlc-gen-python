use ruff_python_ast as ast;

use super::{Attribute, documentation, record};
use crate::api::Api;
use crate::api::models::{Enum, Model};
use crate::python::vocabulary::{CLASSMETHOD, CLS, ENUM, FROM_ARRAY, LABEL, LIST, SELF, STR, TEXT};
use crate::python::{
    Expression, Function, Scope, assign, class, docstring, each, forward, int, list_of, minus, name, parameter,
    receiver, return_, string,
};

/// The classes every query module shares (`docs/python-output.md` §4).
pub fn module(api: &Api) -> Vec<ast::Stmt> {
    let arrays = api.text_array_enums();
    (api.enums.iter())
        .map(|sql_enum| enum_class(sql_enum, arrays.contains(sql_enum.name.as_str())))
        .chain(api.models.iter().map(model_class))
        .collect()
}

/// A `str`-mixin enum, so every driver binds a member as its label, as `enum.StrEnum` would need Python 3.11. Without
/// the `__str__` override, `str(member)` changes between Python 3.10 and 3.12!
///
/// Where the driver returns an array of it as the array's text, the enum parses it (`from_array`). Its labels need no
/// quoting there, so the text is its labels between braces, split by commas.
fn enum_class(sql_enum: &Enum, from_array: bool) -> ast::Stmt {
    let members = (sql_enum.members.iter()).map(|member| assign(&member.name, string(&member.value)));
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
        [STR.into(), ENUM.attr("Enum")],
        (documentation(&sql_enum.comment).into_iter())
            .chain(members)
            .chain([str_method.into()])
            .chain(from_array),
    )
}

fn model_class(model: &Model) -> ast::Stmt {
    let attributes: Vec<Attribute> = (model.fields.iter().enumerate())
        .map(|(index, field)| Attribute {
            comment: field.comment.clone(),
            ..Attribute::column(field, index, Scope::Models)
        })
        .collect();
    record(&model.name, &model.comment, &attributes)
}
