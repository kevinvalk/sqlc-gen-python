use ruff_python_ast as ast;
use ruff_text_size::TextRange;

use super::{identifier, unparse};

pub fn name(id: impl AsRef<str>) -> ast::Expr {
    named(id.as_ref(), ast::ExprContext::Load)
}

/// A name that is bound rather than read, so it asks for no import ([`crate::python::imports`]).
pub(super) fn target(id: impl AsRef<str>) -> ast::Expr {
    named(id.as_ref(), ast::ExprContext::Store)
}

fn named(id: &str, ctx: ast::ExprContext) -> ast::Expr {
    node!(ast::ExprName {
        id: ast::name::Name::new(id),
        ctx: ctx,
    })
    .into()
}

pub fn none() -> ast::Expr {
    node!(ast::ExprNoneLiteral {}).into()
}

pub fn true_() -> ast::Expr {
    node!(ast::ExprBooleanLiteral { value: true }).into()
}

pub fn false_() -> ast::Expr {
    node!(ast::ExprBooleanLiteral { value: false }).into()
}

pub fn ellipsis() -> ast::Expr {
    node!(ast::ExprEllipsisLiteral {}).into()
}

pub fn int(value: usize) -> ast::Expr {
    node!(ast::ExprNumberLiteral {
        value: ast::Number::Int(ast::Int::from(value as u64)),
    })
    .into()
}

pub fn string(value: &str) -> ast::Expr {
    node!(ast::ExprStringLiteral {
        value: ast::StringLiteralValue::single(node!(ast::StringLiteral {
            value: value.into(),
            flags: ast::StringLiteralFlags::empty().with_quote_style(ast::str::Quote::Double),
        })),
    })
    .into()
}

/// A forward reference, as a string, for a class that is still being defined.
pub fn forward(annotation: impl Into<ast::Expr>) -> ast::Expr {
    string(&unparse(&annotation.into()))
}

pub fn tuple(items: Vec<ast::Expr>) -> ast::Expr {
    node!(ast::ExprTuple {
        elts: items,
        ctx: ast::ExprContext::Load,
        parenthesized: true,
    })
    .into()
}

pub fn list(items: Vec<ast::Expr>) -> ast::Expr {
    node!(ast::ExprList {
        elts: items,
        ctx: ast::ExprContext::Load,
    })
    .into()
}

pub fn dict(items: Vec<(ast::Expr, ast::Expr)>) -> ast::Expr {
    node!(ast::ExprDict {
        items: (items.into_iter())
            .map(|(key, value)| ast::DictItem { key: Some(key), value })
            .collect(),
    })
    .into()
}

pub fn starred(value: impl Into<ast::Expr>) -> ast::Expr {
    node!(ast::ExprStarred {
        value: Box::new(value.into()),
        ctx: ast::ExprContext::Load,
    })
    .into()
}

pub fn minus(value: impl Into<ast::Expr>) -> ast::Expr {
    node!(ast::ExprUnaryOp {
        op: ast::UnaryOp::USub,
        operand: Box::new(value.into()),
    })
    .into()
}

pub fn keyword(name: impl AsRef<str>, value: impl Into<ast::Expr>) -> ast::Keyword {
    node!(ast::Keyword {
        arg: Some(identifier(name)),
        value: value.into(),
    })
}

/// `None if value is None else then`, for a `then` that reads a nullable `value`.
pub fn unless_none(value: impl Into<ast::Expr>, then: impl Into<ast::Expr>) -> ast::Expr {
    none_if(value.absent(), then)
}

pub fn none_if(test: impl Into<ast::Expr>, then: impl Into<ast::Expr>) -> ast::Expr {
    node!(ast::ExprIf {
        test: Box::new(test.into()),
        body: Box::new(none()),
        orelse: Box::new(then.into()),
    })
    .into()
}

/// `[item for <each>]`.
pub fn list_of(item: impl Into<ast::Expr>, each: Each) -> ast::Expr {
    node!(ast::ExprListComp {
        elt: Box::new(item.into()),
        generators: vec![node!(ast::Comprehension {
            target: target(&each.target),
            iter: each.iter,
            ifs: each.test.into_iter().collect(),
            is_async: each.is_async,
        })],
    })
    .into()
}

/// `(item for <each>)`, which reads `each` only as it is iterated.
pub fn generator_of(item: impl Into<ast::Expr>, each: Each) -> ast::Expr {
    node!(ast::ExprGenerator {
        elt: Box::new(item.into()),
        generators: vec![node!(ast::Comprehension {
            target: target(&each.target),
            iter: each.iter,
            ifs: each.test.into_iter().collect(),
            is_async: each.is_async,
        })],
        parenthesized: true,
    })
    .into()
}

/// `for target in iter`, of a comprehension ([`list_of`]) or a loop ([`super::for_`]).
pub struct Each {
    pub(super) target: String,
    pub(super) iter: ast::Expr,
    test: Option<ast::Expr>,
    pub(super) is_async: bool,
}

pub fn each(target: impl AsRef<str>, iter: impl Into<ast::Expr>) -> Each {
    Each {
        target: target.as_ref().into(),
        iter: iter.into(),
        test: None,
        is_async: false,
    }
}

impl Each {
    /// `... if test`, in a comprehension.
    pub fn when(self, test: impl Into<ast::Expr>) -> Self {
        Self {
            test: Some(test.into()),
            ..self
        }
    }

    /// `async for`, for a coroutine whose `iter` is asynchronous.
    pub fn asynchronous(self, is_async: bool) -> Self {
        Self { is_async, ..self }
    }
}

/// Builds on an expression in the order Python reads it, so `CUR.attr("execute").call([statement])` is
/// `cur.execute(statement)`.
pub trait Expression: Into<ast::Expr> {
    fn attr(self, name: impl AsRef<str>) -> ast::Expr {
        node!(ast::ExprAttribute {
            value: Box::new(self.into()),
            attr: identifier(name),
            ctx: ast::ExprContext::Load,
        })
        .into()
    }

    fn call_with(
        self,
        args: impl IntoIterator<Item = ast::Expr>,
        keywords: impl IntoIterator<Item = ast::Keyword>,
    ) -> ast::Expr {
        ast::ExprCall {
            node_index: ast::AtomicNodeIndex::default(),
            range_start: TextRange::default().start(),
            func: Box::new(self.into()),
            arguments: node!(ast::Arguments {
                args: args.into_iter().collect(),
                keywords: keywords.into_iter().collect(),
            }),
        }
        .into()
    }

    fn call(self, args: impl IntoIterator<Item = ast::Expr>) -> ast::Expr {
        self.call_with(args, [])
    }

    fn index(self, index: impl Into<ast::Expr>) -> ast::Expr {
        node!(ast::ExprSubscript {
            value: Box::new(self.into()),
            slice: Box::new(index.into()),
            ctx: ast::ExprContext::Load,
        })
        .into()
    }

    /// `self[a, b]`, such as `dict[str, int]`, rather than an index by a tuple.
    fn index_each(self, indexes: Vec<ast::Expr>) -> ast::Expr {
        self.index(node!(ast::ExprTuple {
            elts: indexes,
            ctx: ast::ExprContext::Load,
            parenthesized: false,
        }))
    }

    fn slice(self, lower: impl Into<ast::Expr>, upper: impl Into<ast::Expr>) -> ast::Expr {
        self.index(node!(ast::ExprSlice {
            lower: Some(Box::new(lower.into())),
            upper: Some(Box::new(upper.into())),
            step: None,
        }))
    }

    fn union(self, other: impl Into<ast::Expr>) -> ast::Expr {
        binary(self, ast::Operator::BitOr, other)
    }

    /// `self | None`, unless it already is a union with `None`, as an override's type of a JSON column can be (a JSON
    /// `null` is `None`).
    fn or_none(self) -> ast::Expr {
        fn has_none(annotation: &ast::Expr) -> bool {
            match annotation {
                ast::Expr::NoneLiteral(_) => true,
                ast::Expr::BinOp(union) => has_none(&union.left) || has_none(&union.right),
                _ => false,
            }
        }
        let annotation = self.into();
        match has_none(&annotation) {
            true => annotation,
            false => annotation.union(none()),
        }
    }

    fn times(self, other: impl Into<ast::Expr>) -> ast::Expr {
        binary(self, ast::Operator::Mult, other)
    }

    fn subtract(self, other: impl Into<ast::Expr>) -> ast::Expr {
        binary(self, ast::Operator::Sub, other)
    }

    fn or(self, other: impl Into<ast::Expr>) -> ast::Expr {
        node!(ast::ExprBoolOp {
            op: ast::BoolOp::Or,
            values: vec![self.into(), other.into()],
        })
        .into()
    }

    fn not_(self) -> ast::Expr {
        node!(ast::ExprUnaryOp {
            op: ast::UnaryOp::Not,
            operand: Box::new(self.into()),
        })
        .into()
    }

    fn equals(self, other: impl Into<ast::Expr>) -> ast::Expr {
        self.compare(ast::CmpOp::Eq, other)
    }

    fn compare(self, op: ast::CmpOp, other: impl Into<ast::Expr>) -> ast::Expr {
        node!(ast::ExprCompare {
            ops: Box::new([op]),
            operands: Box::new([self.into(), other.into()]),
        })
        .into()
    }

    /// `self is None`.
    fn absent(self) -> ast::Expr {
        node!(ast::ExprCompare {
            ops: Box::new([ast::CmpOp::Is]),
            operands: Box::new([self.into(), none()]),
        })
        .into()
    }

    /// `await self` in a coroutine and just `self` in a blocking function.
    fn awaited(self, asynchronous: bool) -> ast::Expr {
        match asynchronous {
            true => node!(ast::ExprAwait {
                value: Box::new(self.into()),
            })
            .into(),
            false => self.into(),
        }
    }

    /// `self as name`, of a `with`.
    fn entered_as(self, name: impl AsRef<str>) -> ast::WithItem {
        node!(ast::WithItem {
            context_expr: self.into(),
            optional_vars: Some(Box::new(target(name))),
        })
    }

    /// `self`, of a `with` that binds nothing.
    fn entered(self) -> ast::WithItem {
        node!(ast::WithItem {
            context_expr: self.into(),
            optional_vars: None,
        })
    }
}

impl<T: Into<ast::Expr>> Expression for T {}

fn binary(left: impl Into<ast::Expr>, op: ast::Operator, right: impl Into<ast::Expr>) -> ast::Expr {
    node!(ast::ExprBinOp {
        left: Box::new(left.into()),
        op: op,
        right: Box::new(right.into()),
    })
    .into()
}
