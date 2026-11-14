//! Specification for the syntax generated code is built from.

use super::*;

fn written(stmt: &ast::Stmt) -> String {
    Generator::new(&Indentation::default(), LineEnding::Lf).stmt(stmt)
}

#[test]
fn a_string_literal_is_quoted_to_avoid_escaping() {
    // Data is built rather than parsed precisely so ruff escapes it; here it
    // switches quote style instead, as Black does.
    assert_eq!(python_source(&string("open")), r#""open""#);
    assert_eq!(
        python_source(&string(r#"a "quoted" value"#)),
        r#"'a "quoted" value'"#
    );
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
