use std::sync::LazyLock;

use ruff_python_ast as ast;
use ruff_python_ast::visitor::{Visitor, walk_expr, walk_stmt};
use ruff_python_formatter::{PyFormatOptions, format_module_source};
use ruff_python_parser::parse_module;
use ruff_text_size::TextRange;

use super::imports::imports;
use super::vocabulary::TYPE_CHECKING;
use super::{unparse as unparse_expr, unparse_stmt};

#[cfg(test)]
#[path = "render_spec.rs"]
mod spec;

/// Renders `body` after the imports it reads, of the [vocabulary](super::vocabulary) and of an override's `modules`,
/// with `models` from the module the user named, if not the package's own.
///
/// NOTE: ruff keeps `Generator::generate` `pub(crate)`, so we unparse each top-level statement on its own. The
/// formatter decides the blank lines between them anyway, except for two a tree cannot carry and we write ourselves:
/// the one between two sections of imports, and the one after an `if` that sets apart what follows its last branch.
///
/// We parse the unparsed text back to find its multi-line strings, which only fails when we built invalid Python. That
/// is our bug and never the user's schema (we reject unusable SQL names where names are made), so we panic.
pub fn render(body: &[ast::Stmt], modules: &[String], models: Option<&str>) -> String {
    let unparse = |stmts: &[ast::Stmt]| -> String {
        (stmts.iter())
            .map(|stmt| match stmt {
                ast::Stmt::If(_) => unparse_stmt(stmt) + "\n\n",
                _ => unparse_stmt(stmt) + "\n",
            })
            .collect()
    };
    let [runtime, checked] = imports(body, modules, models);
    let runtime: Vec<String> = runtime.iter().map(|section| unparse(section)).collect();
    // One block for the type checker's imports, its sections apart as at the top
    let checked: Vec<String> = checked.iter().map(|section| unparse(section)).collect();
    let checked = match checked.is_empty() {
        true => String::new(),
        false => {
            let indented: String = (checked.join("\n").lines())
                .map(|line| format!("    {line}\n"))
                .collect();
            format!("if {}:\n{indented}\n", unparse_expr(&TYPE_CHECKING.into()))
        }
    };
    let unparsed = runtime.join("\n") + &checked + &unparse(body);
    format_module_source(&respell_strings(&unparsed), PyFormatOptions::default())
        .expect("respelt strings are valid Python")
        .into_code()
}

/// Whether ruff's S105 would take a string assigned to a name like `PASSWORD` or `_GET_API_TOKEN` for a hardcoded
/// secret. Generated code only assigns SQL and enum labels, so such a module exempts itself from the rule, but only
/// then.
pub fn trips_s105(body: &[ast::Stmt]) -> bool {
    // Taken from ruff's `flake8_bandit::helpers`
    static SECRET: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"(^|_)(?i)(pas+wo?r?d|pass(phrase)?|pwd|token|secrete?)($|_)").expect("ruff's pattern")
    });
    body.iter().any(|stmt| {
        let (targets, value): (Vec<&ast::Expr>, Option<&ast::Expr>) = match stmt {
            ast::Stmt::Assign(assign) => (assign.targets.iter().collect(), Some(&assign.value)),
            ast::Stmt::AnnAssign(assign) => (vec![&assign.target], assign.value.as_deref()),
            ast::Stmt::ClassDef(class) => return trips_s105(&class.body),
            _ => return false,
        };
        let string = value
            .and_then(ast::Expr::as_string_literal_expr)
            .is_some_and(|string| !string.value.to_str().is_empty());
        string
            && (targets.iter())
                .filter_map(|target| target.as_name_expr())
                .any(|name| SECRET.is_match(name.id.as_str()))
    })
}

/// Respells every docstring, and every other string holding a line break, as triple-quoted. ruff's unparser writes a
/// line break as `\n`, but the lines of a docstring and of a SQL statement belong on lines of their own.
///
/// We write each docstring finished (PEP 257), as the formatter re-indents a class or function docstring but leaves an
/// attribute docstring as written. Any other string is data, such as the SQL a query runs, so it stays verbatim. Every
/// string statement we generate is a docstring.
fn respell_strings(source: &str) -> String {
    #[derive(Default)]
    struct Strings(Vec<(TextRange, String, bool)>);
    impl<'a> Visitor<'a> for Strings {
        fn visit_stmt(&mut self, stmt: &'a ast::Stmt) {
            if let ast::Stmt::Expr(ast::StmtExpr { value, range, .. }) = stmt
                && let ast::Expr::StringLiteral(string) = &**value
            {
                return self.0.push((*range, string.value.to_str().into(), true));
            }
            walk_stmt(self, stmt);
        }
        fn visit_expr(&mut self, expr: &'a ast::Expr) {
            if let ast::Expr::StringLiteral(string) = expr
                && string.value.to_str().contains('\n')
            {
                return self.0.push((string.range, string.value.to_str().into(), false));
            }
            walk_expr(self, expr);
        }
    }

    let mut strings = Strings::default();
    let parsed = parse_module(source).unwrap_or_else(|error| panic!("generated invalid Python ({error}):\n{source}"));
    strings.visit_body(&parsed.syntax().body);

    // We replace the last first, so a replacement moves no range we still have to replace. We sort first as a visitor
    // walks in evaluation order (a conditional's test before its body) and not in the source's.
    strings.0.sort_by_key(|(range, ..)| range.start());
    let mut respelt = source.to_string();
    for (range, text, docstring) in strings.0.into_iter().rev() {
        let start = range.start().to_usize();
        let column = start - source[..start].rfind('\n').map_or(0, |newline| newline + 1);
        respelt.replace_range(range.to_std_range(), &triple_quoted(&text, docstring.then_some(column)));
    }
    respelt
}

/// A docstring is written at the column given; any other text verbatim.
///
/// We only escape what a triple-quoted string cannot hold as written, what linters reject even inside one, and what a
/// reader cannot see:
/// - a backslash, and a quote that would close the string early
/// - whatever Python's `str.isprintable()` rejects but a space or a tab, as ruff's unparser does in any other string:
///   control and format characters (the bidi controls of "Trojan Source", PLE2502), and separators an editor breaks a
///   line at (U+2028) or shows as a space (U+00A0)
/// - whitespace ending a line (W291), though a docstring's lines lose it instead
/// - a tab in the indentation of a line that also indents with spaces (E101), as a docstring's continuation lines do
fn triple_quoted(text: &str, docstring: Option<usize>) -> String {
    let lines: Vec<&str> = match docstring {
        Some(_) => text.lines().map(str::trim_end).collect(),
        None => text.split('\n').collect(),
    };
    let indent = " ".repeat(docstring.unwrap_or_default());

    let mut source = String::from(r#"""""#);
    for (n, line) in lines.iter().enumerate() {
        let last = n + 1 == lines.len();
        let indented = n > 0 && !indent.is_empty() && !line.is_empty();
        if n > 0 {
            source.push('\n');
        }
        if indented {
            source += &indent;
        }
        let indentation = &line[..line.len() - line.trim_start_matches([' ', '\t']).len()];
        let mixed = indentation.contains('\t') && (indented || indentation.contains(' '));
        let mut chars = line.char_indices().peekable();
        while let Some((i, c)) = chars.next() {
            let next = chars.peek().map(|&(_, next)| next);
            let ends_line = next.is_none() && !last;
            match c {
                '\\' => source += r"\\",
                '"' if next == Some('"') || (next.is_none() && last) => source += r#"\""#,
                '\t' if (mixed && i < indentation.len()) || ends_line => source += r"\t",
                ' ' if ends_line => source += r"\x20",
                ' ' | '\t' => source.push(c),
                c if !is_printable(c) => {
                    source += &match u32::from(c) {
                        code @ ..=0xff => format!(r"\x{code:02x}"),
                        code @ ..=0xffff => format!(r"\u{code:04x}"),
                        code => format!(r"\U{code:08x}"),
                    }
                }
                c => source.push(c),
            }
        }
    }
    if docstring.is_some() && lines.len() > 1 {
        source += &format!("\n{indent}");
    }
    source + r#"""""#
}

/// Python's `str.isprintable()`
fn is_printable(c: char) -> bool {
    use icu_properties::props::{EnumeratedProperty, GeneralCategory as G};
    c == ' '
        || !matches!(
            G::for_char(c),
            G::Control
                | G::Format
                | G::Surrogate
                | G::PrivateUse
                | G::Unassigned
                | G::LineSeparator
                | G::ParagraphSeparator
                | G::SpaceSeparator
        )
}
