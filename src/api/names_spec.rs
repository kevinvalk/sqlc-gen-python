//! Specification for the Python names of SQL names in one scope.

use super::*;
use crate::api::fixtures::catalog::*;
use crate::api::fixtures::*;
use crate::backend::Backend;
use crate::config::Config;
use crate::plugin;

fn try_field_names(columns: &[&str]) -> Result<Vec<String>> {
    let columns = columns
        .iter()
        .map(|name| column(name, "text", true))
        .collect();
    let resolved = try_resolve(public(vec![table("t", columns)]))?;
    Ok((resolved.models[0].fields.iter())
        .map(|f| f.name.clone())
        .collect())
}

fn field_names(columns: &[&str]) -> Vec<String> {
    try_field_names(columns).unwrap()
}

fn try_member_names(values: &[&str]) -> Result<Vec<String>> {
    let sql_enum = plugin::Enum {
        name: "op".into(),
        vals: values.iter().map(|v| v.to_string()).collect(),
        comment: String::new(),
    };
    let resolved = try_resolve(vec![sql_schema("public", vec![], vec![sql_enum])])?;
    Ok((resolved.enums[0].members.iter())
        .map(|m| m.name.clone())
        .collect())
}

#[test]
fn a_field_is_the_snake_case_spelling_of_its_column() {
    assert_eq!(
        field_names(&["id", "authorID", "parent        id", "Weird Col"]),
        ["id", "author_id", "parent_id", "weird_col"]
    );
}

#[test]
fn a_column_named_like_a_keyword_takes_a_trailing_underscore() {
    // PEP 8's convention; `class: str` would not parse.
    assert_eq!(
        field_names(&["class", "if", "None"]),
        ["class_", "if_", "none"]
    );
}

#[test]
fn a_column_named_like_a_name_generated_code_reads_takes_a_trailing_underscore() {
    // docs/python-output.md §3, rule 1, written out rather than read from
    // `RESERVED`, so a name dropped from it fails here. In a class body,
    // `str: str` makes every later `str` annotation mean the field to mypy and
    // pyright, and `classmethod` the decorator of `from_row`; every module the
    // generated code imports is a name it reads, and `conn`, `row` and `value`
    // are its locals.
    let reserved = [
        "dataclasses",
        "dt",
        "enum",
        "models",
        "postgresql",
        "sqlalchemy",
        "bool",
        "bytes",
        "classmethod",
        "conn",
        "float",
        "from_row",
        "int",
        "list",
        "object",
        "row",
        "str",
        "value",
    ];
    let renamed: Vec<String> = reserved.iter().map(|name| format!("{name}_")).collect();
    assert_eq!(field_names(&reserved), renamed);
    // Classes are imported by their PascalCase name, which a snake_case field
    // cannot take, so `uuid` and `decimal` are free; `id` is §3, rule 2.
    // And `rows` is a parameter only of a function whose others are a
    // record's fields.
    assert_eq!(
        field_names(&["uuid", "decimal", "datetime", "id", "rows"]),
        ["uuid", "decimal", "datetime", "id", "rows"]
    );
}

#[test]
fn a_column_sqlc_left_unnamed_is_named_by_position_as_sqlc_does() {
    // sqlc's Go codegen names an unnamed column `column_<n>`, counting from one.
    assert_eq!(field_names(&["id", ""]), ["id", "column_2"]);
}

#[test]
fn a_column_with_no_usable_spelling_is_an_error_naming_it() {
    for name in ["?!", "1st", "größe", "ﬁle"] {
        let error = error_of(try_field_names(&["id", name, "file"]));
        assert!(
            error.contains(&format!("\"{name}\" does not become")),
            "{error}"
        );
    }
}

#[test]
fn columns_that_spell_alike_are_an_error_not_a_silent_rename() {
    // Either would silently replace the other in the class body.
    let error = error_of(try_field_names(&["user_id", "userId"]));
    assert!(
        error.contains(r#""user_id" and "userId" both become the Python name "user_id""#),
        "{error}"
    );
    let error = error_of(try_field_names(&["from", "from_"]));
    assert!(error.contains(r#"the Python name "from_""#), "{error}");
}

#[test]
fn an_enum_member_is_its_labels_words_in_upper_snake_case() {
    assert_eq!(
        try_member_names(&[
            "direct-debit",
            "a/b:c",
            "inProgress",
            "in shock",
            "x.y",
            "v1"
        ])
        .unwrap(),
        [
            "DIRECT_DEBIT",
            "A_B_C",
            "IN_PROGRESS",
            "IN_SHOCK",
            "X_Y",
            "V1"
        ]
    );
    // A separator at either end separates nothing.
    assert_eq!(try_member_names(&["_x_", "-a"]).unwrap(), ["X", "A"]);
}

#[test]
fn an_enum_label_is_spelt_as_the_options_rename_it() {
    // The only way to name a label that is no identifier, such as a size.
    let sizes = plugin::Enum {
        name: "size".into(),
        vals: vec!["xl".into(), "2xl".into()],
        comment: String::new(),
    };
    let config = Config {
        rename: [("2xl".to_string(), "xxl".to_string())].into(),
        ..Config::of(Backend::SqlAlchemy)
    };
    let api = resolve_options(vec![sql_schema("public", vec![], vec![sizes])], &config).unwrap();
    let members: Vec<(&str, &str)> = (api.enums[0].members.iter())
        .map(|member| (member.name.as_str(), member.value.as_str()))
        .collect();
    assert_eq!(members, [("XL", "xl"), ("XXL", "2xl")]);
}

#[test]
fn an_enum_label_with_no_usable_spelling_is_an_error_naming_it() {
    // Nothing left, or not an identifier.
    for label in ["=", "", "1st", "é x"] {
        let error = error_of(try_member_names(&["open", label]));
        assert!(
            error.contains(&format!("\"{label}\" does not become")),
            "{label}: {error}"
        );
    }
}

#[test]
fn enum_labels_that_spell_alike_are_an_error_not_a_numbered_member() {
    // A repeated member name is a `TypeError` when the enum class is created,
    // and Go's `value_<i>` fallback would be a member nobody could guess.
    let error = error_of(try_member_names(&["a-b", "a_b"]));
    assert!(
        error.contains(r#""a-b" and "a_b" both become the Python name "A_B""#),
        "{error}"
    );
}
