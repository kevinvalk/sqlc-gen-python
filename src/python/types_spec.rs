use super::*;

#[test]
fn an_override_names_only_types_generated_code_can_import_as_written() {
    let error = |python_type: &str| {
        Annotation::try_from(python_type.to_string())
            .err()
            .map(|e| format!("{e:#}"))
    };
    for (bad, problem) in [
        ("numpy.typing.NDArray[numpy.float64]", "ICN001"),
        ("pandas.Series", "ICN001"),
        ("matplotlib.pyplot.Figure", "ICN001"),
        ("datetime.date", "write `dt.date`"),
        ("int, str", "is no type"),
        ("1 + 2", "is no type"),
        ("models.Account", "generated code binds"),
        ("sqlalchemy.Text", "generated code binds"),
        ("dt.date.max", "generated code binds"),
        ("typing.Any", "write `Any`"),
        ("Sequence[int]", "neither a builtin"),
    ] {
        let error = error(bad).unwrap_or_default();
        assert!(error.contains(problem), "{bad}: {error}");
    }
    for good in [
        "dt.date",
        "Decimal",
        "UUID",
        "list[app.T] | None",
        "tuple[float, ...]",
        "IPv4Network",
        "Any",
        "list[Any] | None",
        // ruff aliases exactly the modules it lists, so not a submodule of one.
        "matplotlib.figure.Figure",
        "pandas.api.types.CategoricalDtype",
    ] {
        assert_eq!(error(good), None, "{good}");
    }
}
