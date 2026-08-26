//! Specification for writing Python: fragments, syntax, types and imports.

use super::*;
use ruff_python_codegen::{Generator, Indentation};
use ruff_source_file::LineEnding;

fn shown(expr: &ast::Expr) -> String {
    Generator::new(&Indentation::default(), LineEnding::Lf).expr(expr)
}

fn written(stmt: &ast::Stmt) -> String {
    Generator::new(&Indentation::default(), LineEnding::Lf).stmt(stmt)
}

// -- Fragments ---------------------------------------------------------------

#[test]
fn a_parsed_expression_round_trips() {
    // Fixed Python is written, not built, so it has to survive the round trip.
    for source in [
        "str",
        "enum.Enum",
        "dataclasses.dataclass()",
        "sqlalchemy.ext.asyncio.AsyncConnection",
    ] {
        assert_eq!(shown(&expr(source)), source);
    }
}

#[test]
fn a_parsed_statement_round_trips() {
    for source in ["import datetime", "from typing import Any, Optional"] {
        assert_eq!(written(&stmt(source)), source);
    }
}

// -- Syntax ------------------------------------------------------------------

#[test]
fn a_subscript_annotates_a_generic() {
    assert_eq!(
        shown(&subscript(name("Optional"), expr("decimal.Decimal"))),
        "Optional[decimal.Decimal]"
    );
}

#[test]
fn a_string_literal_is_quoted_to_avoid_escaping() {
    // Data is built rather than parsed precisely so ruff escapes it; here it
    // switches quote style instead, as Black does.
    assert_eq!(shown(&string("open")), r#""open""#);
    assert_eq!(
        shown(&string(r#"a "quoted" value"#)),
        r#"'a "quoted" value'"#
    );
}

#[test]
fn a_docstring_is_a_triple_quoted_string_statement() {
    assert_eq!(written(&docstring("An author.")), r#""""An author.""""#);
}

#[test]
fn an_annotated_assignment_may_carry_a_default() {
    assert_eq!(written(&ann_assign("id", name("int"), None)), "id: int");
    assert_eq!(
        written(&ann_assign("bio", name("str"), Some(name("None")))),
        "bio: str = None"
    );
}

#[test]
fn a_class_without_bases_omits_the_parentheses() {
    let built = class("Author", [], [], [ann_assign("id", name("int"), None)]);
    assert_eq!(written(&built), "class Author:\n    id: int");
}

#[test]
fn a_class_with_bases_lists_them() {
    let built = class("Status", [], [expr("str"), expr("enum.Enum")], []);
    assert!(written(&built).starts_with("class Status(str, enum.Enum):"));
}

#[test]
fn a_class_carries_its_decorators() {
    let built = class("Author", [expr("dataclasses.dataclass()")], [], []);
    assert!(written(&built).starts_with("@dataclasses.dataclass()\nclass Author:"));
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
    let rendered = render(&module([assign("X", string("y"))])).unwrap();
    assert_eq!(rendered, "X = \"y\"\n");
}

// -- Types -------------------------------------------------------------------

fn named(ty: &Type, scope: Scope<'_>) -> String {
    shown(&ty.expr(scope))
}

#[test]
fn a_builtin_is_written_bare_and_needs_no_import() {
    assert_eq!(named(&Type::INT, Scope::Models), "int");
    assert_eq!(Type::INT.import(), None);
}

#[test]
fn a_qualified_type_is_reached_through_the_module_it_imports() {
    assert_eq!(named(&Type::DECIMAL, Scope::Models), "decimal.Decimal");
    assert_eq!(Type::DECIMAL.import(), Some(Import::module("decimal")));
}

#[test]
fn a_named_type_is_written_bare_and_imported_by_name() {
    assert_eq!(named(&Type::OPTIONAL, Scope::Models), "Optional");
    assert_eq!(
        Type::OPTIONAL.import(),
        Some(Import::from("typing", "Optional"))
    );
}

#[test]
fn a_generated_class_is_bare_in_the_models_module_and_qualified_outside_it() {
    let status = Type::Generated("Status".into());
    let queries = Scope::Queries {
        models_module: "models",
    };

    assert_eq!(named(&status, Scope::Models), "Status");
    assert_eq!(named(&status, queries), "models.Status");
    // It is generated alongside, so there is nothing to import.
    assert_eq!(status.import(), None);
}

// -- Imports -----------------------------------------------------------------

fn statements(imports: &Imports) -> Vec<String> {
    imports.statements().iter().map(written).collect()
}

#[test]
fn nothing_is_imported_until_something_needs_it() {
    assert!(statements(&Imports::default()).is_empty());
}

#[test]
fn the_same_import_is_only_recorded_once() {
    let imports: Imports = std::iter::repeat_n(Import::module("datetime"), 3).collect();
    assert_eq!(statements(&imports), ["import datetime"]);
}

#[test]
fn plain_imports_come_before_from_imports() {
    let imports: Imports = [Import::from("typing", "Optional"), Import::module("uuid")]
        .into_iter()
        .collect();

    assert_eq!(
        statements(&imports),
        ["import uuid", "from typing import Optional"]
    );
}

#[test]
fn imports_are_sorted_regardless_of_the_order_they_were_recorded() {
    let imports: Imports = ["uuid", "dataclasses", "decimal"]
        .map(Import::module)
        .into_iter()
        .collect();

    assert_eq!(
        statements(&imports),
        ["import dataclasses", "import decimal", "import uuid"]
    );
}

#[test]
fn names_from_one_module_merge_into_a_single_statement() {
    let imports: Imports = [
        Import::from("typing", "Optional"),
        Import::from("decimal", "Decimal"),
        Import::from("typing", "Any"),
    ]
    .into_iter()
    .collect();

    assert_eq!(
        statements(&imports),
        [
            "from decimal import Decimal",
            "from typing import Any, Optional"
        ]
    );
}
