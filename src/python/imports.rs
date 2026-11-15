//! The imports that make a module's names resolve: never declared, but read
//! off the names its body reads ([`super::vocabulary`]), so a name and its
//! import cannot drift apart.

use std::collections::BTreeSet;

use ruff_python_ast as ast;
use ruff_python_ast::visitor::{Visitor, walk_expr};

use super::vocabulary::{Import, VOCABULARY};
use super::{import, import_from};

#[cfg(test)]
#[path = "imports_spec.rs"]
mod spec;

/// The imports of a module whose body reads `body`'s names, a section at a
/// time: the [vocabulary](VOCABULARY) it reads, with names from one module
/// merged, and each of `modules` it reads a name from, in the standard
/// library's section or the third-party one.
pub(super) fn imports(body: &[ast::Stmt], modules: &[String]) -> Vec<Vec<ast::Stmt>> {
    // The names the body reads, and the dotted ones in full: `a`, `a.b.C`.
    #[derive(Default)]
    struct Reads(BTreeSet<String>);
    impl<'a> Visitor<'a> for Reads {
        fn visit_expr(&mut self, expr: &'a ast::Expr) {
            match expr {
                ast::Expr::Name(name) if name.ctx.is_load() => {
                    self.0.insert(name.id.to_string());
                }
                ast::Expr::Attribute(_) => {
                    self.0.extend(dotted(expr));
                }
                _ => {}
            }
            walk_expr(self, expr);
        }
    }
    fn dotted(expr: &ast::Expr) -> Option<String> {
        match expr {
            ast::Expr::Name(name) => Some(name.id.to_string()),
            ast::Expr::Attribute(attribute) => {
                Some(format!("{}.{}", dotted(&attribute.value)?, attribute.attr))
            }
            _ => None,
        }
    }
    let mut reads = Reads::default();
    reads.visit_body(body);
    // A module is read where a name is read from it: `app.types.Embedding`
    // reads `app.types`, and another module's `app.names.Name` does not.
    let reads_from = |module: &str| {
        let prefix = format!("{module}.");
        reads.0.iter().any(|read| read.starts_with(&prefix))
    };
    let stdlib = |module: &str| {
        let root = module.split('.').next().unwrap_or(module);
        ruff_python_stdlib::sys::is_known_standard_library(10, root)
    };

    (VOCABULARY.iter().enumerate())
        .map(|(at, section)| {
            // isort's order: each `import` by its module, then each `from`.
            let mut plain: Vec<(&str, ast::Stmt)> = Vec::new();
            let mut froms: Vec<(&str, Vec<&str>)> = Vec::new();
            for entry in section.iter().filter(|i| reads.0.contains(i.binding())) {
                match *entry {
                    Import::Module(module, _) => {
                        // `import a.b` binds `a`.
                        let bound = module.split('.').next();
                        let alias = Some(entry.binding()).filter(|alias| Some(*alias) != bound);
                        plain.push((module, import(module, alias)));
                    }
                    Import::From(module, _) => match froms.last_mut() {
                        Some((last, names)) if *last == module => names.push(entry.binding()),
                        _ => froms.push((module, vec![entry.binding()])),
                    },
                }
            }
            for module in modules {
                let section = if stdlib(module) { 0 } else { 1 };
                if section == at && reads_from(module) && !plain.iter().any(|(m, _)| m == module) {
                    plain.push((module, import(module, None)));
                }
            }
            plain.sort_by_key(|(module, _)| module.to_lowercase());
            let froms = (froms.iter()).map(|(module, names)| import_from(module, names));
            (plain.into_iter().map(|(_, import)| import))
                .chain(froms)
                .collect()
        })
        .filter(|section: &Vec<ast::Stmt>| !section.is_empty())
        .collect()
}
