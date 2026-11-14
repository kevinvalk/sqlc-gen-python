//! Specification for the import block.

use ruff_python_stdlib::sys::is_known_standard_library;

use super::*;
use crate::python::{render, stmt};

/// The imports of a module whose body reads `reads`.
fn imports_for(reads: &str) -> String {
    let rendered = render(&[stmt(&format!("x = {reads}"))], &[]);
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
