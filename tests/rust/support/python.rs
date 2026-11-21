use ruff_python_ast::{Expr, Operator, Stmt, StmtClassDef};
use sqlc_gen_python::plugin;
pub(super) use sqlc_gen_python::python::{unparse, unparse_stmt};

/// The record `annotation` names and its fields, if it names one: a class of the query module, or `models.X`.
pub(super) fn record<'a>(
    annotation: &Expr,
    body: &'a [Stmt],
    models: &'a [Stmt],
) -> Option<(&'a StmtClassDef, Vec<(&'a str, &'a Expr)>)> {
    let (name, scope) = match annotation {
        Expr::Name(name) => (name.id.as_str(), body),
        Expr::Attribute(attribute) if is_name(&attribute.value, "models") => (attribute.attr.as_str(), models),
        _ => return None,
    };
    let class = (scope.iter().filter_map(Stmt::as_class_def_stmt))
        .find(|class| class.name.as_str() == name && is_record(class))?;
    Some((class, fields(class)))
}

pub(super) fn fields(class: &StmtClassDef) -> Vec<(&str, &Expr)> {
    (class.body.iter().filter_map(Stmt::as_ann_assign_stmt))
        .filter_map(|field| Some((field.target.as_name_expr()?.id.as_str(), &*field.annotation)))
        .collect()
}

/// A record, a dataclass or a pydantic model.
fn is_record(class: &StmtClassDef) -> bool {
    class.bases().iter().map(unparse).eq(["pydantic.BaseModel"])
        || (class.decorator_list.iter()).any(|decorator| {
            let callee = match &decorator.expression {
                Expr::Call(call) => &call.func,
                other => other,
            };
            callee.as_attribute_expr().map(|a| a.attr.as_str()) == Some("dataclass")
        })
}

pub(super) fn is_name(expr: &Expr, id: &str) -> bool {
    expr.as_name_expr().is_some_and(|name| name.id.as_str() == id)
}

/// The docstring `stmt` is, or empty when it is no string statement.
pub(super) fn docstring(stmt: Option<&Stmt>) -> String {
    (stmt.and_then(Stmt::as_expr_stmt))
        .and_then(|stmt| stmt.value.as_string_literal_expr())
        .map_or(String::new(), |string| string.value.to_str().into())
}

pub(super) fn words(text: &str) -> Vec<&str> {
    text.split_whitespace().collect()
}

/// `X` of `pydantic.SkipValidation[X]`, which is `X` to a type checker, or the annotation.
pub(super) fn unwrapped(annotation: &Expr) -> &Expr {
    match annotation {
        Expr::Subscript(subscript) if unparse(&subscript.value) == "pydantic.SkipValidation" => &subscript.slice,
        annotation => annotation,
    }
}

/// `X` of an annotation `X | None`, or the annotation.
pub(super) fn strip_optional(annotation: &Expr) -> &Expr {
    match annotation {
        Expr::BinOp(union) if union.right.is_none_literal_expr() => &union.left,
        annotation => annotation,
    }
}

/// An error a `:one` raises, which lives with the models, as the one module every query module reads.
pub(super) fn is_error(class: &StmtClassDef) -> bool {
    class.bases().iter().map(unparse).eq(["LookupError"])
}

pub(super) fn is_enum(class: &StmtClassDef) -> bool {
    class.bases().iter().map(unparse).eq(["enum.StrEnum"])
}

/// What a column's annotation says about NULL and arrays.
#[derive(Debug, Eq)]
pub(super) struct Shape {
    pub(super) optional: bool,
    pub(super) depth: usize,
    /// `Any` or `object` alone, which `unknown_type` writes for the whole column, as a driver may return an array of an
    /// unknown type as its text: any depth is its. JSON, the other `object`, is never an array.
    pub(super) unknown: bool,
}

impl PartialEq for Shape {
    fn eq(&self, other: &Self) -> bool {
        self.optional == other.optional && (self.unknown || other.unknown || self.depth == other.depth)
    }
}

impl From<&plugin::Column> for Shape {
    fn from(column: &plugin::Column) -> Self {
        Self {
            optional: !column.not_null,
            depth: column.array_dims as usize,
            unknown: false,
        }
    }
}

impl Shape {
    /// Reads `list[list[T]] | None` as optional, two deep.
    pub(super) fn of(annotation: &Expr) -> Self {
        fn union(expr: &Expr) -> Vec<&Expr> {
            match expr {
                Expr::BinOp(binary) if binary.op == Operator::BitOr => {
                    [union(&binary.left), union(&binary.right)].concat()
                }
                other => vec![other],
            }
        }
        let members = union(unwrapped(annotation));
        let (nones, others): (Vec<_>, Vec<_>) = members.into_iter().partition(|member| member.is_none_literal_expr());
        let depth = match others[..] {
            [Expr::Subscript(list)] if is_name(&list.value, "list") => 1 + Self::of(&list.slice).depth,
            _ => 0,
        };
        Self {
            optional: !nones.is_empty(),
            depth,
            unknown: matches!(others[..], [Expr::Name(name)] if ["Any", "object"].contains(&name.id.as_str())),
        }
    }
}
