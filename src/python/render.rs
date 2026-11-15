//! A tree as a module's source: ruff unparses it, then ruff formats it, with
//! each docstring and multi-line string spelt as one is read.

use std::sync::LazyLock;

use ruff_python_ast as ast;
use ruff_python_ast::visitor::{Visitor, walk_expr, walk_stmt};
use ruff_python_formatter::{PyFormatOptions, format_module_source};
use ruff_python_parser::parse_module;
use ruff_text_size::TextRange;

use super::imports::imports;
use super::unparse_stmt;

#[cfg(test)]
#[path = "render_spec.rs"]
mod spec;

/// Renders a module of `body`, after the imports it reads, of the
/// [vocabulary](super::vocabulary) and of `modules` (an override's): ruff
/// unparses the tree, ruff formats the result.
///
/// `Generator::generate` is `pub(crate)` to ruff, so there is no whole-module
/// entry point that hands back its buffer; each top-level statement is unparsed
/// on its own. Nothing is lost by that — the blank lines between them are the
/// formatter's decision either way, but for two a tree cannot carry and the
/// formatter leaves as written: the one between two sections of imports, and
/// the one after an `if`, which sets apart what follows its last branch.
///
/// The unparsed text is parsed back to find its multi-line strings, which fails
/// exactly when the tree was not valid Python. That is a bug in this crate,
/// never a property of the user's schema — unusable SQL names are handled
/// where names are made — so it panics.
pub fn render(body: &[ast::Stmt], modules: &[String]) -> String {
    let unparse = |stmts: &[ast::Stmt]| -> String {
        (stmts.iter())
            .map(|stmt| match stmt {
                ast::Stmt::If(_) => unparse_stmt(stmt) + "\n\n",
                _ => unparse_stmt(stmt) + "\n",
            })
            .collect()
    };
    let imports: Vec<String> = (imports(body, modules).iter())
        .map(|section| unparse(section))
        .collect();
    let unparsed = imports.join("\n") + &unparse(body);
    format_module_source(&respell_strings(&unparsed), PyFormatOptions::default())
        .expect("respelt strings are valid Python")
        .into_code()
}

/// Whether ruff's S105, which takes a string assigned to a name like
/// `PASSWORD` or `_GET_API_TOKEN` for a hardcoded secret, flags anything in
/// `body`: what generated code assigns is SQL and enum labels, never a
/// secret, so its module exempts itself from the rule, and only then.
pub fn trips_s105(body: &[ast::Stmt]) -> bool {
    // ruff's own pattern (`flake8_bandit::helpers`).
    static SECRET: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"(^|_)(?i)(pas+wo?r?d|pass(phrase)?|pwd|token|secrete?)($|_)")
            .expect("ruff's pattern")
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

/// Spells every docstring, and every other string holding a line break, as
/// one is read: ruff's unparser writes a line break in a string as `\n`, but
/// a docstring's lines, and a SQL statement's, belong on lines of their own.
///
/// A docstring's continuation lines are indented to its own column, and a
/// multi-line docstring closes on a line of its own (PEP 257). The formatter
/// would re-indent a class or function docstring, but leaves an attribute
/// docstring as written, so all are written finished. Any other string is
/// written verbatim: its value is data, such as the SQL a query runs.
///
/// Every string statement this crate generates is a docstring.
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
                return self
                    .0
                    .push((string.range, string.value.to_str().into(), false));
            }
            walk_expr(self, expr);
        }
    }

    let mut strings = Strings::default();
    let parsed = parse_module(source)
        .unwrap_or_else(|error| panic!("generated invalid Python ({error}):\n{source}"));
    strings.visit_body(&parsed.syntax().body);

    // Last first, so that a replacement moves no range still to replace; and
    // sorted, as a visitor walks in evaluation order (a conditional's test
    // before its body), not the source's.
    strings.0.sort_by_key(|(range, ..)| range.start());
    let mut respelt = source.to_string();
    for (range, text, docstring) in strings.0.into_iter().rev() {
        let start = range.start().to_usize();
        let column = start - source[..start].rfind('\n').map_or(0, |newline| newline + 1);
        respelt.replace_range(
            range.to_std_range(),
            &triple_quoted(&text, docstring.then_some(column)),
        );
    }
    respelt
}

/// The source of a triple-quoted string holding `text`: a docstring written at
/// the column given, or else `text` verbatim.
///
/// Only what a triple-quoted string cannot hold as written is escaped, plus
/// what linters reject even inside one, or a reader cannot see: a backslash; a
/// quote that would close the string early; whatever Python's
/// `str.isprintable()` rejects other than a space or a tab, as ruff's
/// unparser does in any other string (control and format characters, such as
/// the bidi controls of "Trojan Source", PLE2502; separators an editor breaks
/// a line at, U+2028, or shows as a space, U+00A0); whitespace ending a line
/// (W291); and a tab in the indentation of a line that also indents with
/// spaces (E101), which a docstring's continuation lines always do. A
/// docstring's lines lose their trailing whitespace instead.
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

/// Whether Python's `str.isprintable()` holds of `c`: not a control, format,
/// private use, unassigned or separator character, the ASCII space excepted.
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
