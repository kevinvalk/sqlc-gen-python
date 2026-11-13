//! Specification for writing Python: fragments, syntax and imports.

use super::*;
use ruff_python_codegen::{Generator, Indentation};
use ruff_python_stdlib::sys::is_known_standard_library;
use ruff_source_file::LineEnding;

fn written(stmt: &ast::Stmt) -> String {
    Generator::new(&Indentation::default(), LineEnding::Lf).stmt(stmt)
}

fn unparse(expr: &ast::Expr) -> String {
    Generator::new(&Indentation::default(), LineEnding::Lf).expr(expr)
}

// -- Syntax ------------------------------------------------------------------

#[test]
fn a_string_literal_is_quoted_to_avoid_escaping() {
    // Data is built rather than parsed precisely so ruff escapes it; here it
    // switches quote style instead, as Black does.
    assert_eq!(unparse(&string("open")), r#""open""#);
    assert_eq!(
        unparse(&string(r#"a "quoted" value"#)),
        r#"'a "quoted" value'"#
    );
}

fn documented(docstrings: &[&str]) -> String {
    let body = docstrings
        .iter()
        .flat_map(|text| [ann_assign("x", expr("int")), docstring(text)]);
    render(&[class("A", [], [], body)])
}

/// The docstrings a rendered module holds, read back by ruff's parser.
fn docstrings_of(source: &str) -> Vec<String> {
    let parsed = parse_module(source).unwrap().into_syntax();
    let ast::Stmt::ClassDef(class) = &parsed.body[0] else {
        panic!("{source}")
    };
    (class.body.iter())
        .filter_map(|stmt| stmt.as_expr_stmt()?.value.as_string_literal_expr())
        .map(|string| string.value.to_str().to_string())
        .collect()
}

#[test]
fn a_docstring_is_a_triple_quoted_string_statement() {
    assert_eq!(
        documented(&["A field."]),
        "class A:\n    x: int\n    \"\"\"A field.\"\"\"\n"
    );
}

#[test]
fn a_multi_line_docstring_is_indented_to_its_own_column_and_closes_on_its_own_line() {
    // PEP 257 (D209). The formatter leaves an attribute docstring as written.
    assert_eq!(
        documented(&["First.\nSecond.\n\nFourth."]),
        "class A:\n    x: int\n    \"\"\"First.\n    Second.\n\n    Fourth.\n    \"\"\"\n"
    );
}

#[test]
fn a_docstring_holds_its_text_exactly_whatever_it_contains() {
    let texts = [
        r#"May quote "a reference", \ included."#,
        r#"Ends with a quote: "x""#,
        r#"Holds """ triple quotes."#,
        r"Ends with a backslash \",
        "Tab\there, bell\u{7}, carriage\rreturn, NUL\u{0}.",
        "\"Starts with a quote.",
    ];
    assert_eq!(docstrings_of(&documented(&texts)), texts);
}

#[test]
fn a_docstring_escapes_only_what_a_triple_quoted_string_cannot_hold() {
    assert_eq!(
        documented(&[r#"Say "hi" \ bye."#]),
        "class A:\n    x: int\n    \"\"\"Say \"hi\" \\\\ bye.\"\"\"\n"
    );
}

#[test]
fn a_docstring_escapes_whitespace_a_reader_cannot_see() {
    // A line separator would split the line in an editor (ruff RUF002), and a
    // no-break space reads as a space.
    let text = "a\u{2028}b\u{2029}c\u{a0}d\u{85}e";
    let source = documented(&[text]);
    assert!(
        source.contains(r#""""a\u2028b\u2029c\xa0d\x85e""""#),
        "{source}"
    );
    assert_eq!(docstrings_of(&source), [text]);
}

#[test]
fn a_docstring_drops_whitespace_ending_a_line_rather_than_escaping_it() {
    // A docstring is prose: trailing whitespace carries nothing, where in SQL
    // it is part of the text (W291 either way).
    assert_eq!(
        documented(&["Ends in spaces  \nand a no-break space\u{a0}\nend"]),
        "class A:\n    x: int\n    \"\"\"Ends in spaces\n    and a no-break space\n    end\n    \"\"\"\n"
    );
}

/// What Python's `str.isprintable()` rejects, apart from space, tab and line
/// breaks: format characters (zero-width space, BOM, the bidi controls of
/// "Trojan Source"), private use and unassigned code points, outside the
/// Basic Multilingual Plane too.
const UNPRINTABLE: &str = "a\u{200b}b\u{feff}c\u{202e}d\u{2066}e\u{e000}f\u{e0001}g\u{10ffff}h";
const UNPRINTABLE_ESCAPED: &str = r"a\u200bb\ufeffc\u202ed\u2066e\ue000f\U000e0001g\U0010ffffh";

#[test]
fn a_docstring_escapes_what_python_does_not_print() {
    // ruff PLE2502 and PLE2515; a reader cannot see them.
    let source = documented(&[UNPRINTABLE, "naïve – “quoted”"]);
    assert!(
        source.contains(&format!("\"\"\"{UNPRINTABLE_ESCAPED}\"\"\"")),
        "{source}"
    );
    assert!(source.contains("\"\"\"naïve – “quoted”\"\"\""), "{source}");
    assert_eq!(docstrings_of(&source), [UNPRINTABLE, "naïve – “quoted”"]);
}

#[test]
fn a_docstring_escapes_a_tab_that_would_follow_its_own_indentation() {
    // The docstring's spaces then the comment's tab are mixed indentation
    // (E101), whether the formatter re-indents the docstring or not.
    let source = documented(&["x\n\ty"]);
    assert!(source.contains("\"\"\"x\n    \\ty\n    \"\"\""), "{source}");
    // The value is still a tab, after the indentation `inspect.cleandoc` drops.
    assert_eq!(docstrings_of(&source), ["x\n    \ty\n    "]);

    // A backslash keeps the formatter from re-indenting a function's (D206).
    let mut function = stmt("def f() -> None:\n    pass");
    if let ast::Stmt::FunctionDef(definition) = &mut function {
        definition.body.insert(0, docstring("a\n\tb\nc\\"));
    }
    let source = render(&[function]);
    assert!(source.contains("\n    \\tb\n"), "{source}");
}

/// How a module holding `x = f(<value>)` renders.
fn rendered_string(value: &str) -> String {
    let call = call(expr("f"), [string(value)]);
    render(&[assign("X", call)])
}

#[test]
fn a_string_with_a_line_break_is_triple_quoted_and_written_verbatim() {
    assert_eq!(
        rendered_string("SELECT a\n  FROM t"),
        "X = f(\"\"\"SELECT a\n  FROM t\"\"\")\n"
    );
}

#[test]
fn a_verbatim_string_escapes_what_a_triple_quoted_string_cannot_hold_or_linters_reject() {
    // A backslash, a quote before the closing ones, a control character, and
    // whitespace ending a line (W291).
    assert_eq!(
        rendered_string("a\\:b \nc\r\n\"d\""),
        "X = f(\"\"\"a\\\\:b\\x20\nc\\x0d\n\"d\\\"\"\"\")\n"
    );
}

#[test]
fn a_verbatim_string_escapes_the_tabs_of_an_indentation_that_mixes_them_with_spaces() {
    // E101 reads a string's lines as indentation too.
    assert_eq!(
        rendered_string("a\n\tb\n \tc"),
        "X = f(\"\"\"a\n\tb\n \\tc\"\"\")\n"
    );
}

#[test]
fn a_verbatim_string_escapes_what_python_does_not_print() {
    let source = rendered_string(&format!("SELECT '{UNPRINTABLE}'\n"));
    assert_eq!(
        source,
        format!("X = f(\"\"\"SELECT '{UNPRINTABLE_ESCAPED}'\n\"\"\")\n")
    );
}

#[test]
fn a_string_without_a_line_break_is_left_to_ruff() {
    assert_eq!(rendered_string("a 'b'"), "X = f(\"a 'b'\")\n");
}

#[test]
fn a_class_without_bases_omits_the_parentheses() {
    let built = class("Author", [], [], [ann_assign("id", name("int"))]);
    assert_eq!(written(&built), "class Author:\n    id: int");
}

#[test]
fn an_empty_class_body_becomes_pass() {
    // Python has no empty suite, so something has to stand in for one.
    assert_eq!(
        written(&class("Empty", [], [], [])),
        "class Empty:\n    pass"
    );
}

#[test]
fn a_module_renders_through_ruffs_formatter() {
    let rendered = render(&[assign("X", string("y"))]);
    assert_eq!(rendered, "X = \"y\"\n");
}

// -- Imports -----------------------------------------------------------------

/// The imports of a module whose body reads `reads`.
fn imports_for(reads: &str) -> String {
    let rendered = render(&[stmt(&format!("x = {reads}"))]);
    rendered[..rendered.find("x = ").unwrap()].trim_end().into()
}

#[test]
fn nothing_is_imported_until_something_is_read() {
    assert_eq!(imports_for("int"), "");
}

#[test]
fn a_module_imports_exactly_the_vocabulary_its_body_reads() {
    assert_eq!(
        imports_for("[UUID, uuid, Decimal(), models.X, unknown]"),
        "from decimal import Decimal\nfrom uuid import UUID\n\nfrom . import models"
    );
}

#[test]
fn datetime_is_imported_as_dt_by_ruffs_import_conventions() {
    assert_eq!(imports_for("dt.date"), "import datetime as dt");
}

#[test]
fn plain_imports_come_before_from_imports_and_one_modules_names_merge() {
    assert_eq!(
        imports_for("(Any, enum, IPv6Network, IPv4Address, dataclasses)"),
        "import dataclasses\nimport enum\nfrom ipaddress import IPv4Address, IPv6Network\nfrom typing import Any"
    );
}

#[test]
fn sections_go_standard_library_third_party_relative_with_a_blank_line_between() {
    assert_eq!(
        imports_for("(models, AsyncConnection, sqlalchemy, Sequence, postgresql)"),
        "from collections.abc import Sequence\n\n\
         import sqlalchemy\n\
         from sqlalchemy.dialects import postgresql\n\
         from sqlalchemy.ext.asyncio import AsyncConnection\n\n\
         from . import models"
    );
}

#[test]
fn the_vocabulary_is_written_in_isorts_sections_and_order() {
    // As ruff's isort sections them, for the oldest supported Python.
    let section = |module: &str| match module.split('.').next().unwrap_or_default() {
        "" => 2,
        top if is_known_standard_library(10, top) => 0,
        _ => 1,
    };
    for (n, imports) in VOCABULARY.iter().enumerate() {
        assert!(imports.is_sorted(), "section {n} is not in isort's order");
        for import in *imports {
            let (Import::Module(module) | Import::From(module, _)) = *import;
            assert_eq!(section(module), n, "{import:?}");
        }
    }
}

#[test]
fn an_imported_name_is_one_nothing_generated_may_define() {
    for name in ["Decimal", "UUID", "Sequence", "Any", "dt", "models"] {
        assert!(is_imported(name), "{name}");
    }
    assert!(!is_imported("datetime"));
}

#[test]
fn a_public_name_is_an_ascii_identifier_that_is_neither_a_keyword_nor_private() {
    for name in ["id", "user_id", "VALUE_0", "Author", "match", "type"] {
        assert!(is_public_name(name), "{name}");
    }
    // Not ASCII: ruff PLC2401, and Python NFKC-normalises identifiers, so
    // `ﬁle` (a ligature) and `file` would be one name.
    for name in [
        "", "1st", "user id", "a-b", "class", "None", "_id", "__x", "_sunder_", "naïve", "ﬁle",
    ] {
        assert!(!is_public_name(name), "{name}");
    }
}

#[test]
#[should_panic(expected = "generated invalid Python")]
fn rendering_a_tree_that_is_not_python_is_a_bug_not_an_error() {
    render(&[assign("class", string("y"))]);
}
