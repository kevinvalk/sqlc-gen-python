//! Specification for the import block.

use super::*;
use crate::python::vocabulary::{ASYNCMY, DATE, MODELS};
use crate::python::{Expression, assign, name, render, tuple};

/// The imports of a module whose body reads `reads`.
fn imports_for(reads: Vec<ast::Expr>) -> String {
    let rendered = render(&[assign("x", tuple(reads))], &[]);
    rendered[..rendered.find("x = ").unwrap()].trim_end().into()
}

#[test]
fn nothing_is_imported_until_something_is_read() {
    assert_eq!(imports_for(vec![name("int")]), "");
}

#[test]
fn a_module_imports_exactly_the_vocabulary_its_body_reads() {
    assert_eq!(
        imports_for(vec![
            name("UUID"),
            name("uuid"),
            name("Decimal").call([]),
            MODELS.attr("X"),
            name("unknown"),
        ]),
        "from decimal import Decimal\nfrom uuid import UUID\n\nfrom . import models"
    );
}

#[test]
fn a_name_that_is_bound_is_not_read() {
    // An enum's member `UUID` is not the class its module would import.
    let rendered = render(&[assign("UUID", name("int"))], &[]);
    assert_eq!(rendered, "UUID = int\n");
}

#[test]
fn datetime_is_imported_as_dt_by_ruffs_import_conventions() {
    assert_eq!(imports_for(vec![DATE.into()]), "import datetime as dt");
}

#[test]
fn a_submodule_is_imported_by_its_path_which_binds_its_package() {
    // No `as`: `import asyncmy.cursors` binds `asyncmy`, the name read.
    assert_eq!(
        imports_for(vec![ASYNCMY.attr("cursors").attr("Cursor")]),
        "import asyncmy.cursors"
    );
    assert_eq!(
        imports_for(vec![ASYNCMY.attr("Connection")]),
        "import asyncmy.cursors"
    );
}

#[test]
fn plain_imports_come_before_from_imports_and_one_modules_names_merge() {
    assert_eq!(
        imports_for(
            ["Any", "enum", "IPv6Network", "IPv4Address", "dataclasses"]
                .map(name)
                .to_vec()
        ),
        "import dataclasses\nimport enum\nfrom ipaddress import IPv4Address, IPv6Network\nfrom typing import Any"
    );
}

#[test]
fn sections_go_standard_library_third_party_relative_with_a_blank_line_between() {
    assert_eq!(
        imports_for(
            [
                "models",
                "AsyncConnection",
                "sqlalchemy",
                "Sequence",
                "postgresql"
            ]
            .map(name)
            .to_vec()
        ),
        "from collections.abc import Sequence\n\n\
         import sqlalchemy\n\
         from sqlalchemy.dialects import postgresql\n\
         from sqlalchemy.ext.asyncio import AsyncConnection\n\n\
         from . import models"
    );
}

#[test]
fn an_overrides_module_is_imported_only_where_a_name_is_read_from_it() {
    let modules = ["app.names".to_string(), "app.types".to_string()];
    let annotation = name("app").attr("types").attr("Title");
    let rendered = render(&[assign("x", annotation)], &modules);
    assert_eq!(rendered, "import app.types\n\nx = app.types.Title\n");
}
