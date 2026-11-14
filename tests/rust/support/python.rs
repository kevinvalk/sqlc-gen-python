//! Reading the generated Python: what a class, an annotation or a statement
//! of a parsed module says, for the invariants to hold against sqlc's request.

use ruff_python_ast::{Expr, Operator, Stmt, StmtClassDef};
use sqlc_gen_python::plugin;

/// The source of an expression, as ruff writes it.
pub(super) fn unparse(expr: &Expr) -> String {
    ruff_python_codegen::Generator::new(
        &ruff_python_codegen::Indentation::default(),
        ruff_source_file::LineEnding::Lf,
    )
    .expr(expr)
}

/// The record `annotation` names, and its fields, if it names one: a class
/// of the query module, or `models.X`.
pub(super) fn record<'a>(
    annotation: &Expr,
    body: &'a [Stmt],
    models: &'a [Stmt],
) -> Option<(&'a StmtClassDef, Vec<(&'a str, &'a Expr)>)> {
    let (name, scope) = match annotation {
        Expr::Name(name) => (name.id.as_str(), body),
        Expr::Attribute(attribute) if is_name(&attribute.value, "models") => {
            (attribute.attr.as_str(), models)
        }
        _ => return None,
    };
    let class = (scope.iter().filter_map(Stmt::as_class_def_stmt))
        .find(|class| class.name.as_str() == name && is_dataclass(class))?;
    Some((class, fields(class)))
}

/// A dataclass's fields, in order.
pub(super) fn fields(class: &StmtClassDef) -> Vec<(&str, &Expr)> {
    (class.body.iter().filter_map(Stmt::as_ann_assign_stmt))
        .filter_map(|field| Some((field.target.as_name_expr()?.id.as_str(), &*field.annotation)))
        .collect()
}

fn is_dataclass(class: &StmtClassDef) -> bool {
    (class.decorator_list.iter()).any(|decorator| {
        let callee = match &decorator.expression {
            Expr::Call(call) => &call.func,
            other => other,
        };
        callee.as_attribute_expr().map(|a| a.attr.as_str()) == Some("dataclass")
    })
}

pub(super) fn is_name(expr: &Expr, id: &str) -> bool {
    expr.as_name_expr()
        .is_some_and(|name| name.id.as_str() == id)
}

/// The string `stmt` is, if it is a string statement: a docstring.
pub(super) fn docstring(stmt: Option<&Stmt>) -> String {
    (stmt.and_then(Stmt::as_expr_stmt))
        .and_then(|stmt| stmt.value.as_string_literal_expr())
        .map_or(String::new(), |string| string.value.to_str().into())
}

pub(super) fn words(text: &str) -> Vec<&str> {
    text.split_whitespace().collect()
}

/// `X` of an annotation `X | None`, or the annotation.
pub(super) fn strip_optional(annotation: &Expr) -> &Expr {
    match annotation {
        Expr::BinOp(union) if union.right.is_none_literal_expr() => &union.left,
        annotation => annotation,
    }
}

pub(super) fn is_enum(class: &StmtClassDef) -> bool {
    class.bases().iter().map(unparse).eq(["str", "enum.Enum"])
}

/// What a column's annotation says about NULL and arrays.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Shape {
    pub(super) optional: bool,
    pub(super) depth: usize,
}

impl From<&plugin::Column> for Shape {
    /// What sqlc says of a column.
    fn from(column: &plugin::Column) -> Self {
        Self {
            optional: !column.not_null,
            depth: column.array_dims as usize,
        }
    }
}

impl Shape {
    /// Reads `list[list[T]] | None` as optional, two deep.
    pub(super) fn of(annotation: &Expr) -> Self {
        fn union(expr: &Expr) -> Vec<&Expr> {
            match expr {
                Expr::BinOp(op) if op.op == Operator::BitOr => {
                    [union(&op.left), union(&op.right)].concat()
                }
                other => vec![other],
            }
        }
        let members = union(annotation);
        let (none, rest): (Vec<_>, Vec<_>) = members
            .into_iter()
            .partition(|member| member.is_none_literal_expr());
        let depth = match rest[..] {
            [Expr::Subscript(list)] if is_name(&list.value, "list") => {
                1 + Self::of(&list.slice).depth
            }
            _ => 0,
        };
        Self {
            optional: !none.is_empty(),
            depth,
        }
    }
}
