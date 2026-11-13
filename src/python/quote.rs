//! Quasi-quoting: Python written as Python, with schema data spliced in as
//! syntax trees.
//!
//! A template is source this crate writes, holding no data: each `$name` in
//! it is a hole, which [`quote`] fills with the [`Hole`] given for it after
//! the template is parsed. Data therefore never passes through the parser,
//! and the template still reads as the Python it produces:
//!
//! ```text
//! py_expr!("None if $value is None else $class($value)", value = v, class = c)
//! ```
//!
//! What a hole takes depends on where it stands: an expression where an
//! expression stands; a list of them after `*` in a call, list or tuple; a
//! list of keywords after `**` in a call; a list of statements, or one
//! expression, as a statement of its own; a list of parameters as a keyword-only parameter; and
//! a name where an identifier stands (`def $name`, `$class.from_row`,
//! `cls($field=...)`). A hole a template uses but is not given, or one given
//! that the template does not use, is a bug in this crate, and panics.
//!
//! A template is a constant, or put together from this crate's own text
//! alone: `async with` or `with`, as a coroutine's `with` and `for` are
//! async exactly where the driver's are, not everywhere in it.

use std::collections::{BTreeMap, BTreeSet};

use ruff_python_ast::visitor::transformer::{
    Transformer, walk_expr, walk_keyword, walk_parameters, walk_stmt,
};
use ruff_python_ast::{self as ast, name::Name};
use ruff_python_parser::{parse_expression, parse_module};
use thin_vec::ThinVec;

#[cfg(test)]
#[path = "quote_spec.rs"]
mod spec;

/// What fills a hole.
#[derive(Debug, Clone)]
pub enum Hole {
    Expr(ast::Expr),
    Exprs(Vec<ast::Expr>),
    Keywords(Vec<ast::Keyword>),
    Stmts(Vec<ast::Stmt>),
    Params(Vec<ast::ParameterWithDefault>),
    /// An identifier: valid, as every name this crate binds is.
    Name(String),
}

impl From<ast::Expr> for Hole {
    fn from(expr: ast::Expr) -> Self {
        Self::Expr(expr)
    }
}
impl From<Vec<ast::Expr>> for Hole {
    fn from(exprs: Vec<ast::Expr>) -> Self {
        Self::Exprs(exprs)
    }
}
impl From<Vec<ast::Keyword>> for Hole {
    fn from(keywords: Vec<ast::Keyword>) -> Self {
        Self::Keywords(keywords)
    }
}
impl From<Vec<ast::Stmt>> for Hole {
    fn from(stmts: Vec<ast::Stmt>) -> Self {
        Self::Stmts(stmts)
    }
}
impl From<ast::Stmt> for Hole {
    fn from(stmt: ast::Stmt) -> Self {
        Self::Stmts(vec![stmt])
    }
}
impl From<Vec<ast::ParameterWithDefault>> for Hole {
    fn from(params: Vec<ast::ParameterWithDefault>) -> Self {
        Self::Params(params)
    }
}
impl From<&str> for Hole {
    fn from(name: &str) -> Self {
        Self::Name(name.into())
    }
}
impl From<&String> for Hole {
    fn from(name: &String) -> Self {
        Self::Name(name.clone())
    }
}

/// What a template's holes are spelt as once parsed.
const PREFIX: &str = "__hole_";

/// An expression from `template`, its holes filled.
pub fn expr(template: &str, holes: Vec<(&str, Hole)>) -> ast::Expr {
    let (source, filler) = prepare(template, holes);
    let mut expr = *parse_expression(&source)
        .unwrap_or_else(|error| panic!("template `{template}`: {error}"))
        .into_syntax()
        .body;
    filler.visit_expr(&mut expr);
    expr
}

/// Statements from `template`, its holes filled.
pub fn stmts(template: &str, holes: Vec<(&str, Hole)>) -> Vec<ast::Stmt> {
    let (source, filler) = prepare(template, holes);
    let mut body = parse_module(&source)
        .unwrap_or_else(|error| panic!("template `{template}`: {error}"))
        .into_syntax()
        .body;
    filler.splice(&mut body);
    for stmt in &mut body {
        filler.visit_stmt(stmt);
    }
    body.into_iter().collect()
}

/// `template` as Python, each `$name` a placeholder identifier, and dedented
/// if it opens with a line break (so that a template can be indented as the
/// Rust around it is); and the filler of its holes.
fn prepare(template: &str, holes: Vec<(&str, Hole)>) -> (String, Filler) {
    let text = match template.strip_prefix('\n') {
        None => template.to_string(),
        Some(lines) => {
            let indent = (lines.lines().filter(|line| !line.trim().is_empty()))
                .map(|line| line.len() - line.trim_start().len())
                .min()
                .unwrap_or(0);
            (lines.lines())
                .map(|line| line.get(indent..).unwrap_or("").trim_end())
                .collect::<Vec<_>>()
                .join("\n")
        }
    };
    let mut source = String::new();
    let mut used = BTreeSet::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '$' {
            source.push(c);
            continue;
        }
        let mut name = String::new();
        while let Some(&c) = chars
            .peek()
            .filter(|c| c.is_ascii_alphanumeric() || **c == '_')
        {
            name.push(c);
            chars.next();
        }
        assert!(
            !name.is_empty(),
            "template `{template}`: `$` without a name"
        );
        source += PREFIX;
        source += &name;
        used.insert(name);
    }
    let holes: BTreeMap<String, Hole> = (holes.into_iter())
        .map(|(name, hole)| (name.to_string(), hole))
        .collect();
    let given: BTreeSet<String> = holes.keys().cloned().collect();
    assert_eq!(
        used, given,
        "template `{template}`: holes used and given differ"
    );
    (source, Filler { holes })
}

/// Fills a parsed template's holes.
struct Filler {
    holes: BTreeMap<String, Hole>,
}

impl Filler {
    /// The hole `name` spells, if it spells one.
    fn hole(&self, name: &str) -> Option<&Hole> {
        name.strip_prefix(PREFIX).map(|hole| &self.holes[hole])
    }

    /// The identifier a hole at `name` gives, or `name` itself.
    fn identifier(&self, name: &str) -> Option<String> {
        match self.hole(name)? {
            Hole::Name(name) => Some(name.clone()),
            hole => panic!("{hole:?} in an identifier's place"),
        }
    }

    /// The hole a `$name` statement of its own is, if `stmt` is one.
    fn stmt_hole(&self, stmt: &ast::Stmt) -> Option<&Hole> {
        let ast::Stmt::Expr(ast::StmtExpr { value, .. }) = stmt else {
            return None;
        };
        self.hole(value.as_name_expr()?.id.as_str())
    }

    /// Fills every statement hole in `body`, and in the bodies within it.
    fn splice(&self, body: &mut ThinVec<ast::Stmt>) {
        let mut spliced = ThinVec::with_capacity(body.len());
        for mut stmt in std::mem::take(body) {
            match self.stmt_hole(&stmt) {
                Some(Hole::Stmts(stmts)) => spliced.extend(stmts.iter().cloned()),
                // An expression of its own: a call made for its effect.
                Some(Hole::Expr(expr)) => spliced.push(super::expression(expr.clone())),
                Some(hole) => panic!("{hole:?} in a statement's place"),
                None => {
                    self.splice_within(&mut stmt);
                    spliced.push(stmt);
                }
            }
        }
        *body = spliced;
    }

    fn splice_within(&self, stmt: &mut ast::Stmt) {
        match stmt {
            ast::Stmt::FunctionDef(def) => self.splice(&mut def.body),
            ast::Stmt::ClassDef(class) => self.splice(&mut class.body),
            ast::Stmt::With(with) => self.splice(&mut with.body),
            ast::Stmt::For(each) => {
                self.splice(&mut each.body);
                self.splice(&mut each.orelse);
            }
            ast::Stmt::If(branch) => {
                self.splice(&mut branch.body);
                for clause in &mut branch.elif_else_clauses {
                    self.splice(&mut clause.body);
                }
            }
            ast::Stmt::Match(choice) => {
                for case in &mut choice.cases {
                    self.splice(&mut case.body);
                }
            }
            _ => {}
        }
    }

    /// `items`, each `*$hole` replaced by the hole's expressions.
    fn spread(&self, items: Vec<ast::Expr>) -> Vec<ast::Expr> {
        let mut spread = Vec::with_capacity(items.len());
        for item in items {
            let starred = item.as_starred_expr().and_then(|s| s.value.as_name_expr());
            match starred.and_then(|name| self.hole(name.id.as_str())) {
                Some(Hole::Exprs(exprs)) => spread.extend(exprs.iter().cloned()),
                Some(Hole::Expr(_)) | None => spread.push(item),
                Some(hole) => panic!("{hole:?} after `*`"),
            }
        }
        spread
    }
}

impl Transformer for Filler {
    fn visit_stmt(&self, stmt: &mut ast::Stmt) {
        match stmt {
            ast::Stmt::FunctionDef(def) => {
                if let Some(name) = self.identifier(def.name.as_str()) {
                    def.name = ast::Identifier::new(name, def.name.range);
                }
            }
            ast::Stmt::ClassDef(class) => {
                if let Some(name) = self.identifier(class.name.as_str()) {
                    class.name = ast::Identifier::new(name, class.name.range);
                }
            }
            _ => {}
        }
        walk_stmt(self, stmt);
    }

    fn visit_expr(&self, expr: &mut ast::Expr) {
        match expr {
            ast::Expr::Name(name) => match self.hole(name.id.as_str()) {
                Some(Hole::Expr(filled)) => return *expr = filled.clone(),
                Some(Hole::Name(id)) => name.id = Name::new(id),
                Some(hole) => panic!("{hole:?} in an expression's place"),
                None => {}
            },
            ast::Expr::Attribute(attribute) => {
                if let Some(name) = self.identifier(attribute.attr.as_str()) {
                    attribute.attr = ast::Identifier::new(name, attribute.attr.range);
                }
            }
            ast::Expr::Call(call) => {
                let args = std::mem::take(&mut call.arguments.args)
                    .into_iter()
                    .collect();
                call.arguments.args = self.spread(args).into_iter().collect();
                let mut keywords = ThinVec::new();
                for keyword in std::mem::take(&mut call.arguments.keywords) {
                    let spread = (keyword.arg.is_none())
                        .then(|| keyword.value.as_name_expr())
                        .flatten()
                        .and_then(|name| self.hole(name.id.as_str()));
                    match spread {
                        Some(Hole::Keywords(filled)) => keywords.extend(filled.iter().cloned()),
                        Some(hole) => panic!("{hole:?} after `**`"),
                        None => keywords.push(keyword),
                    }
                }
                call.arguments.keywords = keywords;
            }
            ast::Expr::List(list) => list.elts = self.spread(std::mem::take(&mut list.elts)),
            ast::Expr::Tuple(tuple) => tuple.elts = self.spread(std::mem::take(&mut tuple.elts)),
            _ => {}
        }
        walk_expr(self, expr);
    }

    fn visit_keyword(&self, keyword: &mut ast::Keyword) {
        if let Some(arg) = &keyword.arg
            && let Some(name) = self.identifier(arg.as_str())
        {
            keyword.arg = Some(ast::Identifier::new(name, arg.range));
        }
        walk_keyword(self, keyword);
    }

    fn visit_parameters(&self, parameters: &mut ast::Parameters) {
        let mut filled = ThinVec::new();
        for parameter in std::mem::take(&mut parameters.kwonlyargs) {
            match self.hole(parameter.parameter.name.as_str()) {
                Some(Hole::Params(params)) => filled.extend(params.iter().cloned()),
                Some(hole) => panic!("{hole:?} in a parameter's place"),
                None => filled.push(parameter),
            }
        }
        parameters.kwonlyargs = filled;
        walk_parameters(self, parameters);
    }
}
