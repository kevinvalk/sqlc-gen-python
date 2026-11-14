//! Specification for the names generated code reads.

use ruff_python_stdlib::sys::is_known_standard_library;

use super::*;
use crate::python::unparse;

#[test]
fn a_symbol_reads_as_the_name_it_is() {
    assert_eq!(unparse(&ROW.into()), "row");
    assert_eq!(unparse(&DATE.into()), "dt.date");
    assert_eq!(unparse(&LIST.index(INT)), "list[int]");
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
            let (Import::Module(module, _) | Import::From(module, _)) = *import;
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
