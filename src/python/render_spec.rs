use super::*;
use crate::python::vocabulary::{ENUM, FINAL, INT, SQLALCHEMY};
use crate::python::{Expression, Function, ann_assign, annotate, assign, class, docstring, name, none, string};

fn documented(docstrings: &[&str]) -> String {
    let body = docstrings.iter().flat_map(|text| [annotate("x", INT), docstring(text)]);
    render(&[class("A", [], [], body)], &[], None)
}

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
    // PEP 257 (D209), which we have to do ourselves as the formatter leaves an attribute docstring as written
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
    // A line separator would split the line in an editor (ruff RUF002), and a no-break space reads as a space.
    let text = "a\u{2028}b\u{2029}c\u{a0}d\u{85}e";
    let source = documented(&[text]);
    assert!(source.contains(r#""""a\u2028b\u2029c\xa0d\x85e""""#), "{source}");
    assert_eq!(docstrings_of(&source), [text]);
}

#[test]
fn a_docstring_drops_whitespace_ending_a_line_rather_than_escaping_it() {
    // A docstring is prose, so trailing whitespace carries nothing, while in SQL it is part of the text (W291 either
    // way).
    assert_eq!(
        documented(&["Ends in spaces  \nand a no-break space\u{a0}\nend"]),
        "class A:\n    x: int\n    \"\"\"Ends in spaces\n    and a no-break space\n    end\n    \"\"\"\n"
    );
}

/// What Python's `str.isprintable()` rejects apart from space, tab and line breaks: format characters (zero-width
/// space, BOM, the bidi controls of "Trojan Source"), private use and unassigned code points, outside the Basic
/// Multilingual Plane too.
const UNPRINTABLE: &str = "a\u{200b}b\u{feff}c\u{202e}d\u{2066}e\u{e000}f\u{e0001}g\u{10ffff}h";
const UNPRINTABLE_ESCAPED: &str = r"a\u200bb\ufeffc\u202ed\u2066e\ue000f\U000e0001g\U0010ffffh";

#[test]
fn a_docstring_escapes_what_python_does_not_print() {
    // A reader cannot see them (ruff PLE2502 and PLE2515)
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
    // The docstring's spaces and then the text's tab are mixed indentation (E101), whether the formatter re-indents the
    // docstring or not.
    let source = documented(&["x\n\ty"]);
    assert!(source.contains("\"\"\"x\n    \\ty\n    \"\"\""), "{source}");
    // The value is still a tab once `inspect.cleandoc` drops the indentation
    assert_eq!(docstrings_of(&source), ["x\n    \ty\n    "]);

    // A backslash keeps the formatter from re-indenting a function's docstring (D206)
    let function = Function {
        body: vec![docstring("a\n\tb\nc\\")],
        ..Function::new("f", none())
    };
    let source = render(&[function.into()], &[], None);
    assert!(source.contains("\n    \\tb\n"), "{source}");
}

fn rendered_string(value: &str) -> String {
    let call = name("f").call([string(value)]);
    render(&[assign("X", call)], &[], None)
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
    // A backslash, a quote before the closing ones, a control character, and whitespace ending a line (W291)
    assert_eq!(
        rendered_string("a\\:b \nc\r\n\"d\""),
        "X = f(\"\"\"a\\\\:b\\x20\nc\\x0d\n\"d\\\"\"\"\")\n"
    );
}

#[test]
fn a_verbatim_string_escapes_the_tabs_of_an_indentation_that_mixes_them_with_spaces() {
    // E101 reads a string's lines as indentation too.
    assert_eq!(rendered_string("a\n\tb\n \tc"), "X = f(\"\"\"a\n\tb\n \\tc\"\"\")\n");
}

#[test]
fn a_verbatim_string_escapes_what_python_does_not_print() {
    let source = rendered_string(&format!("SELECT '{UNPRINTABLE}'\n"));
    assert_eq!(source, format!("X = f(\"\"\"SELECT '{UNPRINTABLE_ESCAPED}'\n\"\"\")\n"));
}

#[test]
fn a_string_without_a_line_break_is_left_to_ruff() {
    assert_eq!(rendered_string("a 'b'"), "X = f(\"a 'b'\")\n");
}

#[test]
fn a_module_renders_through_ruffs_formatter() {
    let rendered = render(&[assign("X", string("y"))], &[], None);
    assert_eq!(rendered, "X = \"y\"\n");
}

#[test]
#[should_panic(expected = "generated invalid Python")]
fn rendering_a_tree_that_is_not_python_is_a_bug_not_an_error() {
    render(&[assign("class", string("y"))], &[], None);
}

#[test]
fn a_string_assigned_to_a_name_ruff_takes_for_a_secrets_trips_s105() {
    let trips = |stmt: ast::Stmt| trips_s105(&[stmt]);
    let select = || string("SELECT 1");
    assert!(trips(ann_assign("_GET_API_TOKEN", FINAL, select())));
    assert!(trips(class(
        "Role",
        [],
        [ENUM.attr("StrEnum")],
        [assign("PASSWORD", string("password"))],
    )));
    assert!(trips(assign("_UPDATE_USER_PASSWD", string("x"))));
    assert!(!trips(ann_assign("_GET_TOKENS", FINAL, select())));
    assert!(!trips(ann_assign(
        "_GET_API_TOKEN",
        FINAL,
        SQLALCHEMY.attr("text").call([select()]),
    )));
    assert!(!trips(assign("_TOKEN", string(""))));
}
