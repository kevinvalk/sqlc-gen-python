use ruff_python_ast as ast;
use ruff_text_size::TextRange;
use thin_vec::ThinVec;

use super::expression::{Each, string, target};
use super::identifier;

/// What [`stmts!`] puts a body together from: a statement, an optional one, or several.
pub trait Statements {
    fn append_to(self, body: &mut Vec<ast::Stmt>);
}

impl Statements for ast::Stmt {
    fn append_to(self, body: &mut Vec<ast::Stmt>) {
        body.push(self);
    }
}

impl Statements for Option<ast::Stmt> {
    fn append_to(self, body: &mut Vec<ast::Stmt>) {
        body.extend(self);
    }
}

impl Statements for Vec<ast::Stmt> {
    fn append_to(self, body: &mut Vec<ast::Stmt>) {
        body.extend(self);
    }
}

impl Statements for Option<Vec<ast::Stmt>> {
    fn append_to(self, body: &mut Vec<ast::Stmt>) {
        body.extend(self.into_iter().flatten());
    }
}

macro_rules! stmts {
    ($($part:expr),* $(,)?) => {{
        let mut body: Vec<ruff_python_ast::Stmt> = Vec::new();
        $($crate::python::Statements::append_to($part, &mut body);)*
        body
    }};
}
pub(crate) use stmts;

pub fn expression(value: impl Into<ast::Expr>) -> ast::Stmt {
    node!(ast::StmtExpr {
        value: Box::new(value.into()),
    })
    .into()
}

/// Generated code carries its documentation as docstrings because ruff's AST has nowhere to put a `#` comment, and
/// tooling reads a docstring where it would ignore a comment. [`crate::python::render()`] spells it.
pub fn docstring(text: &str) -> ast::Stmt {
    expression(string(text))
}

pub fn return_(value: impl Into<ast::Expr>) -> ast::Stmt {
    node!(ast::StmtReturn {
        value: Some(Box::new(value.into())),
    })
    .into()
}

pub fn yield_(value: impl Into<ast::Expr>) -> ast::Stmt {
    expression(node!(ast::ExprYield {
        value: Some(Box::new(value.into())),
    }))
}

pub fn raise(error: impl Into<ast::Expr>) -> ast::Stmt {
    node!(ast::StmtRaise {
        exc: Some(Box::new(error.into())),
        cause: None,
    })
    .into()
}

/// Release a resource if its setup fails, including when an async caller cancels the operation.
pub fn on_error(body: Vec<ast::Stmt>, cleanup: ast::Stmt) -> ast::Stmt {
    let reraise = node!(ast::StmtRaise { exc: None, cause: None });
    let handler = node!(ast::ExceptHandlerExceptHandler {
        type_: Some(Box::new(super::name("BaseException"))),
        name: None,
        body: vec![cleanup, reraise.into()].into(),
    });
    node!(ast::StmtTry {
        body: body.into(),
        handlers: vec![handler.into()],
        orelse: ThinVec::new(),
        finalbody: ThinVec::new(),
        is_star: false,
    })
    .into()
}

pub fn assign(name: impl AsRef<str>, value: impl Into<ast::Expr>) -> ast::Stmt {
    assign_to(target(name), value)
}

pub fn add_assign(name: impl AsRef<str>, value: impl Into<ast::Expr>) -> ast::Stmt {
    node!(ast::StmtAugAssign {
        target: Box::new(target(name)),
        op: ast::Operator::Add,
        value: Box::new(value.into()),
    })
    .into()
}

/// For a target that is not a bare name, such as `cur.row_factory = None`.
pub fn assign_to(target: ast::Expr, value: impl Into<ast::Expr>) -> ast::Stmt {
    node!(ast::StmtAssign {
        targets: vec![target],
        value: Box::new(value.into()),
    })
    .into()
}

pub fn annotate(name: impl AsRef<str>, annotation: impl Into<ast::Expr>) -> ast::Stmt {
    annotated(name, annotation.into(), None)
}

pub fn ann_assign(name: impl AsRef<str>, annotation: impl Into<ast::Expr>, value: impl Into<ast::Expr>) -> ast::Stmt {
    annotated(name, annotation.into(), Some(value.into()))
}

fn annotated(name: impl AsRef<str>, annotation: ast::Expr, value: Option<ast::Expr>) -> ast::Stmt {
    node!(ast::StmtAnnAssign {
        target: Box::new(target(name)),
        annotation: Box::new(annotation),
        value: value.map(Box::new),
        simple: true,
    })
    .into()
}

pub fn if_(test: impl Into<ast::Expr>, body: Vec<ast::Stmt>) -> ast::Stmt {
    node!(ast::StmtIf {
        test: Box::new(test.into()),
        body: body.into(),
        elif_else_clauses: vec![],
    })
    .into()
}

pub fn if_else(test: impl Into<ast::Expr>, body: Vec<ast::Stmt>, orelse: Vec<ast::Stmt>) -> ast::Stmt {
    node!(ast::StmtIf {
        test: Box::new(test.into()),
        body: body.into(),
        elif_else_clauses: vec![node!(ast::ElifElseClause {
            test: None,
            body: orelse.into(),
        })],
    })
    .into()
}

pub fn for_(each: Each, body: Vec<ast::Stmt>) -> ast::Stmt {
    node!(ast::StmtFor {
        is_async: each.is_async,
        target: Box::new(target(&each.target)),
        iter: Box::new(each.iter),
        body: body.into(),
        orelse: ThinVec::new(),
    })
    .into()
}

pub fn with(asynchronous: bool, items: Vec<ast::WithItem>, body: Vec<ast::Stmt>) -> ast::Stmt {
    node!(ast::StmtWith {
        is_async: asynchronous,
        items: items,
        body: body.into(),
    })
    .into()
}

pub fn match_(subject: impl Into<ast::Expr>, cases: Vec<(ast::Pattern, Vec<ast::Stmt>)>) -> ast::Stmt {
    node!(ast::StmtMatch {
        subject: Box::new(subject.into()),
        cases: (cases.into_iter())
            .map(|(pattern, body)| node!(ast::MatchCase {
                pattern: pattern,
                guard: None,
                body: body.into(),
            }))
            .collect(),
    })
    .into()
}

pub mod pattern {
    use super::*;

    pub fn sequence(patterns: Vec<ast::Pattern>) -> ast::Pattern {
        node!(ast::PatternMatchSequence { patterns: patterns }).into()
    }

    pub fn capture(name: impl AsRef<str>) -> ast::Pattern {
        node!(ast::PatternMatchAs {
            pattern: None,
            name: Some(identifier(name)),
        })
        .into()
    }

    pub fn wildcard() -> ast::Pattern {
        node!(ast::PatternMatchAs {
            pattern: None,
            name: None,
        })
        .into()
    }
}

pub fn parameter(name: impl AsRef<str>, annotation: impl Into<ast::Expr>) -> ast::ParameterWithDefault {
    node!(ast::ParameterWithDefault {
        parameter: node!(ast::Parameter {
            name: identifier(name),
            annotation: Some(Box::new(annotation.into())),
        }),
        default: None,
    })
}

/// `self` or `cls`, which needs no annotation.
pub fn receiver(name: impl AsRef<str>) -> ast::ParameterWithDefault {
    node!(ast::ParameterWithDefault {
        parameter: node!(ast::Parameter {
            name: identifier(name),
            annotation: None,
        }),
        default: None,
    })
}

pub struct Function {
    pub name: String,
    pub decorators: Vec<ast::Expr>,
    pub parameters: Vec<ast::ParameterWithDefault>,
    pub keyword_only: Vec<ast::ParameterWithDefault>,
    pub returns: ast::Expr,
    pub body: Vec<ast::Stmt>,
    pub is_async: bool,
}

impl Function {
    pub fn new(name: impl AsRef<str>, returns: impl Into<ast::Expr>) -> Self {
        Self {
            name: name.as_ref().into(),
            decorators: vec![],
            parameters: vec![],
            keyword_only: vec![],
            returns: returns.into(),
            body: vec![],
            is_async: false,
        }
    }
}

impl From<Function> for ast::Stmt {
    fn from(function: Function) -> Self {
        node!(ast::StmtFunctionDef {
            is_async: function.is_async,
            decorator_list: decorators(function.decorators),
            name: identifier(&function.name),
            type_params: None,
            parameters: Box::new(node!(ast::Parameters {
                posonlyargs: ThinVec::new(),
                args: function.parameters.into(),
                vararg: None,
                kwonlyargs: function.keyword_only.into(),
                kwarg: None,
            })),
            returns: Some(Box::new(function.returns)),
            body: function.body.into(),
        })
        .into()
    }
}

/// An empty base list omits the parentheses and an empty body becomes `pass`, as neither is valid Python written out
/// literally.
pub fn class(
    name: impl AsRef<str>,
    decorated: impl IntoIterator<Item = ast::Expr>,
    bases: impl IntoIterator<Item = ast::Expr>,
    body: impl IntoIterator<Item = ast::Stmt>,
) -> ast::Stmt {
    let bases: ThinVec<ast::Expr> = bases.into_iter().collect();
    let body: ThinVec<ast::Stmt> = body.into_iter().collect();

    node!(ast::StmtClassDef {
        decorator_list: decorators(decorated),
        name: identifier(name),
        type_params: None,
        arguments: (!bases.is_empty()).then(|| {
            Box::new(node!(ast::Arguments {
                args: bases,
                keywords: ThinVec::new(),
            }))
        }),
        body: match body.is_empty() {
            true => thin_vec::thin_vec![node!(ast::StmtPass {}).into()],
            false => body,
        },
    })
    .into()
}

fn decorators(expressions: impl IntoIterator<Item = ast::Expr>) -> ThinVec<ast::Decorator> {
    (expressions.into_iter())
        .map(|expression| node!(ast::Decorator { expression: expression }))
        .collect()
}

pub fn import(module: &str, alias: Option<&str>) -> ast::Stmt {
    node!(ast::StmtImport {
        names: vec![node!(ast::Alias {
            name: identifier(module),
            asname: alias.map(identifier),
        })],
        is_lazy: false,
    })
    .into()
}

/// The module may be relative, such as `.`.
pub fn import_from(module: &str, names: &[&str]) -> ast::Stmt {
    let level = module.chars().take_while(|c| *c == '.').count();
    let module = &module[level..];
    node!(ast::StmtImportFrom {
        module: (!module.is_empty()).then(|| identifier(module)),
        names: (names.iter())
            .map(|name| node!(ast::Alias {
                name: identifier(name),
                asname: None,
            }))
            .collect(),
        level: level as u32,
        is_lazy: false,
    })
    .into()
}
