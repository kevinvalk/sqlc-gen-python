//! The imports that make a module's names resolve: never declared, but read
//! off the names its body reads, so a name and its import cannot drift apart.

use std::collections::BTreeSet;

use ruff_python_ast as ast;
use ruff_python_ast::visitor::{Visitor, walk_expr};

#[cfg(test)]
#[path = "imports_spec.rs"]
mod spec;

/// Everything generated code may read without defining it, and how each is
/// imported (`docs/python-output.md` §2). [`super::render()`] imports exactly the
/// entries its body reads, as written here: in isort's sections (the standard
/// library, third-party, relative), each listing plain `import`s before `from`
/// imports, by module, then by name.
///
/// `datetime` is bound as `dt`, ruff's convention (ICN001): a column called
/// `datetime` or `date` cannot hide it, and `dt.datetime` does not stutter.
const VOCABULARY: [&[Import]; 3] = [
    &[
        Import::Module("contextlib"),
        Import::Module("dataclasses"),
        Import::Module("datetime"),
        Import::Module("enum"),
        Import::Module("sqlite3"),
        Import::From("collections.abc", "Iterable"),
        Import::From("collections.abc", "Sequence"),
        Import::From("decimal", "Decimal"),
        Import::From("ipaddress", "IPv4Address"),
        Import::From("ipaddress", "IPv4Interface"),
        Import::From("ipaddress", "IPv4Network"),
        Import::From("ipaddress", "IPv6Address"),
        Import::From("ipaddress", "IPv6Interface"),
        Import::From("ipaddress", "IPv6Network"),
        Import::From("typing", "Any"),
        Import::From("typing", "Final"),
        Import::From("uuid", "UUID"),
    ],
    &[
        Import::Module("aiosqlite"),
        Import::Module("psycopg"),
        Import::Module("pymysql"),
        Import::Module("sqlalchemy"),
        Import::From("psycopg.rows", "tuple_row"),
        Import::From("psycopg.types.json", "Json"),
        Import::From("psycopg.types.json", "Jsonb"),
        Import::From("psycopg.types.range", "Range"),
        Import::From("sqlalchemy.dialects", "postgresql"),
        Import::From("sqlalchemy.ext.asyncio", "AsyncConnection"),
    ],
    // isort orders classes before modules.
    &[
        Import::From(".", "MultipleRowsError"),
        Import::From(".", "NoRowError"),
        Import::From(".", "models"),
    ],
];

/// Whether generated code imports something under `name`, which nothing it
/// defines may then take.
pub fn is_imported(name: &str) -> bool {
    (VOCABULARY.iter().copied().flatten()).any(|import| import.binding() == name)
}

/// One import statement's worth of a module, ordered as isort orders one
/// section.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Import {
    /// `import <module>`.
    Module(&'static str),
    /// `from <module> import <name>`; `<module>` may be relative, such as `.`.
    From(&'static str, &'static str),
}

impl Import {
    /// The name this import binds.
    fn binding(&self) -> &'static str {
        match *self {
            Self::Module("datetime") => "dt",
            Self::Module(module) => module,
            Self::From(_, name) => name,
        }
    }
}

/// The import block of a module whose body reads `body`'s names: the
/// [vocabulary](VOCABULARY) it reads, with names from one module merged, and
/// each of `modules` whose first name it reads, in the standard library's
/// section or the third-party one; a blank line between sections.
///
/// It is the one part of a module besides the header written as text: every
/// name in it is a constant of this crate or a checked dotted name, and the
/// blank line between sections is layout a tree cannot carry.
pub(super) fn imports(body: &[ast::Stmt], modules: &[String]) -> String {
    #[derive(Default)]
    struct Reads(BTreeSet<String>);
    impl<'a> Visitor<'a> for Reads {
        fn visit_expr(&mut self, expr: &'a ast::Expr) {
            if let ast::Expr::Name(name) = expr
                && name.ctx.is_load()
            {
                self.0.insert(name.id.to_string());
            }
            walk_expr(self, expr);
        }
    }
    let mut reads = Reads::default();
    reads.visit_body(body);

    let sections: Vec<String> = (VOCABULARY.iter().enumerate())
        .map(|(at, section)| {
            // isort's order: each `import` by its module, then each `from`.
            let mut plain: Vec<(String, String)> = Vec::new();
            let mut froms: Vec<String> = Vec::new();
            let mut previous: Option<Import> = None;
            for &import in section.iter().filter(|i| reads.0.contains(i.binding())) {
                match (import, previous) {
                    (Import::From(module, name), Some(Import::From(last, _))) if last == module => {
                        *froms.last_mut().expect("a previous import") += &format!(", {name}");
                    }
                    (Import::From(module, name), _) => {
                        froms.push(format!("from {module} import {name}"));
                    }
                    (Import::Module(module), _) => match import.binding() {
                        alias if alias == module => {
                            plain.push((module.into(), format!("import {module}")))
                        }
                        alias => plain.push((module.into(), format!("import {module} as {alias}"))),
                    },
                }
                previous = Some(import);
            }
            let stdlib = |module: &str| {
                let root = module.split('.').next().unwrap_or(module);
                ruff_python_stdlib::sys::is_known_standard_library(10, root)
            };
            for module in modules {
                let root = module.split('.').next().unwrap_or(module);
                let section = if stdlib(module) { 0 } else { 1 };
                if section == at
                    && reads.0.contains(root)
                    && !plain.iter().any(|(m, _)| m == module)
                {
                    plain.push((module.clone(), format!("import {module}")));
                }
            }
            plain.sort_by_key(|(module, _)| module.to_lowercase());
            (plain.into_iter().map(|(_, line)| line).chain(froms))
                .map(|line| format!("{line}\n"))
                .collect()
        })
        .filter(|section: &String| !section.is_empty())
        .collect();
    sections.join("\n")
}
