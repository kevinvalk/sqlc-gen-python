use std::collections::BTreeSet;

use ruff_python_ast as ast;
use ruff_python_ast::visitor::{Visitor, walk_expr};

use super::vocabulary::{Import, MODELS, VOCABULARY};
use super::{import, import_from};

#[cfg(test)]
#[path = "imports_spec.rs"]
mod spec;

/// We never declare imports but read them off the names a body reads, so a name and its import cannot drift apart. The
/// result is one list per isort section, holding the [vocabulary](VOCABULARY) the body reads and each of an override's
/// `modules` it reads a name from. `models` is the module a package of query modules alone reads its models from, which
/// isort cannot know to be the application's own any more than an override's module.
pub(super) fn imports(body: &[ast::Stmt], modules: &[String], models: Option<&str>) -> Vec<Vec<ast::Stmt>> {
    // We keep dotted names in full (`a.b.C`) so a module's read can be told from another's.
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
            ast::Expr::Attribute(attribute) => Some(format!("{}.{}", dotted(&attribute.value)?, attribute.attr)),
            _ => None,
        }
    }
    let mut reads = Reads::default();
    reads.visit_body(body);
    // `app.types.Embedding` reads `app.types`, but another module's `app.names.Name` does not.
    let reads_from = |module: &str| {
        let prefix = format!("{module}.");
        reads.0.iter().any(|read| read.starts_with(&prefix))
    };
    let stdlib = |module: &str| {
        let root = module.split('.').next().unwrap_or(module);
        ruff_python_stdlib::sys::is_known_standard_library(10, root)
    };

    (VOCABULARY.iter().enumerate())
        .map(|(index, section)| {
            // isort orders each `import` by its module, then each `from`.
            let mut plain: Vec<(&str, ast::Stmt)> = Vec::new();
            let mut froms: Vec<(&str, Vec<&str>)> = Vec::new();
            let is_elsewhere = |entry: &&Import| models.is_some() && entry.binding() == MODELS.as_ref();
            for entry in (section.iter()).filter(|i| reads.0.contains(i.binding()) && !is_elsewhere(i)) {
                match *entry {
                    Import::Module(module, _) => {
                        let bound = module.split('.').next(); // `import a.b` binds `a`
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
                let module_section = if stdlib(module) { 0 } else { 1 };
                if module_section == index
                    && reads_from(module)
                    && !plain.iter().any(|(imported, _)| imported == module)
                {
                    plain.push((module, import(module, None)));
                }
            }
            // `import app.db_models as models`, but `from app import models`, which ruff (PLR0402) asks for over an alias
            if let Some(module) = models
                && index == 1
                && reads.0.contains(MODELS.as_ref())
            {
                match module.rsplit_once('.') {
                    Some((parent, last)) if last == MODELS.as_ref() => froms.push((parent, vec![MODELS.as_ref()])),
                    _ => plain.push((
                        module,
                        import(module, Some(MODELS.as_ref()).filter(|alias| alias != &module)),
                    )),
                }
            }
            plain.sort_by_key(|(module, _)| module.to_lowercase());
            froms.sort_by_key(|(module, _)| module.to_lowercase());
            let froms = (froms.iter()).map(|(module, names)| import_from(module, names));
            (plain.into_iter().map(|(_, import)| import)).chain(froms).collect()
        })
        .filter(|section: &Vec<ast::Stmt>| !section.is_empty())
        .collect()
}
