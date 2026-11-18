use sqlc_gen_python::plugin;

use crate::support;

/// Golden case `case`'s request and files, with the first `old` of each edit replaced by its `new` in `file`.
fn plant(case: &str, file: &str, edits: &[(&str, &str)]) -> (plugin::GenerateRequest, Vec<plugin::File>) {
    let case = support::case(case);
    let mut files = support::generated_files(&case.directory());
    let module = files.iter_mut().find(|f| f.name == file).unwrap();
    let mut source = String::from_utf8(module.contents.clone()).unwrap();
    for (old, new) in edits {
        assert!(source.contains(old), "the planted bug no longer applies: {old}");
        source = source.replacen(old, new, 1);
    }
    module.contents = source.into_bytes();
    (case.request(), files)
}

fn planted_all(case: &str, file: &str, edits: &[(&str, &str)]) -> Vec<String> {
    let (request, files) = plant(case, file, edits);
    support::check_output(&request, &files)
}

fn planted(case: &str, file: &str, old: &str, new: &str) -> Vec<String> {
    planted_all(case, file, &[(old, new)])
}

/// What the invariants say of the invoicing case with its files edited.
fn files_planted(edit: impl FnOnce(&mut Vec<plugin::File>)) -> Vec<String> {
    let case = support::case("invoicing_sqlalchemy");
    let mut files = support::generated_files(&case.directory());
    edit(&mut files);
    support::check_output(&case.request(), &files)
}

fn module(source: &str) -> String {
    support::check_module(source).join("\n")
}

#[test]
fn models_follow_the_columns_nullability_and_array_depth() {
    for (old, new) in [
        ("    small: int\n", "    small: int | None\n"),
        ("    optional: str | None\n", "    optional: str\n"),
        ("grid: list[list[int]] | None", "grid: list[int] | None"),
        ("    tags: list[str]\n", "    tags: str\n"),
    ] {
        let problems = planted("pgtypes_sqlalchemy", "models.py", old, new).join("\n");
        assert!(
            problems.contains("has no table with its fields'"),
            "{new:?} not caught: {problems}"
        );
    }
}

#[test]
fn signatures_follow_sqlcs_nullability_and_array_depth() {
    for (file, old, new, problem) in [
        // A nullable parameter that would no longer accept `None`.
        (
            "customers.py",
            "*, email: str | None = None\n) -> UUID:",
            "*, email: str\n) -> UUID:",
            "parameter `email`",
        ),
        // A `:copyfrom` field that would accept a NULL the column refuses.
        (
            "line_items.py",
            "    quantity: Decimal\n",
            "    quantity: Decimal | None\n",
            "parameter `quantity`",
        ),
        // A row field that hides a NULL.
        (
            "invoices.py",
            "    customer_name: str\n    email: str | None\n",
            "    customer_name: str\n    email: str\n",
            "field `email`",
        ),
        // Bare results: an array flattened, and NULL let into a list.
        (
            "payments.py",
            ") -> list[models.BillingPaymentMethod]:",
            ") -> models.BillingPaymentMethod:",
            "the bare result",
        ),
        (
            "invoices.py",
            "*, customer_id: UUID\n) -> list[str]:",
            "*, customer_id: UUID\n) -> list[str | None]:",
            "the bare result",
        ),
    ] {
        let problems = planted("invoicing_sqlalchemy", file, old, new).join("\n");
        assert!(
            problems.contains(&format!("{problem} disagrees with sqlc")),
            "{new:?} not caught: {problems}"
        );
    }
}

#[test]
fn no_field_mentions_any_at_any_depth() {
    for annotation in ["Any", "typing.Any", "list[typing.Any]", "t.Any | None"] {
        let source =
            format!("import typing\nimport typing as t\nfrom typing import Any\n\n\nclass Row:\n    x: {annotation}\n");
        let problems = module(&source);
        assert!(problems.contains("annotated with `Any`"), "{annotation}: {problems}");
    }
}

#[test]
fn no_parameter_or_result_mentions_any_but_the_drivers_own() {
    // We need this check as pyright strict and mypy --strict accept an explicit `Any`, and ruff's ANN401 only flags a
    // bare one.
    for (file, old, new) in [
        ("invoices.py", "ids: list[UUID]", "ids: list[Any]"),
        ("invoices.py", ") -> list[str]:", ") -> list[Any]:"),
        (
            "invoices.py",
            "row: Sequence[Any]) -> \"GetInvoiceWithCustomerRow\"",
            "row: Any) -> \"GetInvoiceWithCustomerRow\"",
        ),
    ] {
        let problems = planted("invoicing_sqlalchemy", file, old, new).join("\n");
        assert!(problems.contains("with `Any`"), "{new:?} not caught: {problems}");
    }
}

#[test]
fn only_dunders_and_module_constants_may_start_with_an_underscore() {
    assert_eq!(
        module(
            "import enum\n\n_GET_INVOICE = 1\n\n\nclass A(enum.StrEnum):\n    def __str__(self) -> str:\n        return \"\"\n"
        ),
        ""
    );
    for private in [
        "_get_invoice = 1\n",
        "class _Row:\n    pass\n",
        "class A:\n    _X = 1\n",
    ] {
        assert!(module(private).contains("is private"), "{private:?} not caught");
    }
}

#[test]
fn a_prelude_is_its_librarys_to_the_letter_and_no_other_module_has_one() {
    let needs = "its prelude is not the one its library's stubs need";
    for (case, old, new, problem) in [
        // asyncmy's bridge one letter off is just another private class of `Any`s, and half of it is no prelude at all.
        (
            "billing_pymysql_async",
            "async def fetchmany(self, size: int)",
            "async def fetchmany(self, size: Any)",
            "module: `_Cursor` is private",
        ),
        (
            "billing_pymysql_async",
            "def _typed(cursor: asyncmy.cursors.Cursor) -> _Cursor:",
            "def _typed(cursor: Any) -> _Cursor:",
            "parameter `cursor` of `_typed` is annotated with `Any`",
        ),
        (
            "billing_pymysql_async",
            "def _typed(cursor: asyncmy.cursors.Cursor) -> _Cursor:\n    return cursor\n\n\n",
            "",
            needs,
        ),
        // PyMySQL's connection, named any other way, is a private name.
        (
            "billing_pymysql",
            "    _Connection = pymysql.Connection[Any]\n",
            "    _Connection = pymysql.Connection[pymysql.cursors.Cursor]\n",
            "module: `_Connection` is private",
        ),
    ] {
        let problems = planted(case, "query.py", old, new).join("\n");
        assert!(problems.contains(problem), "{new:?} not caught: {problems}");
    }
    // Each prelude is its own library's only: PyMySQL's stubs do type what a statement runs through, and no other
    // connection is generic in its stubs alone.
    for (case, other) in [
        ("billing_pymysql_async", "billing_pymysql"),
        ("billing_pymysql", "billing_sqlalchemy"),
    ] {
        let (_, files) = plant(case, "query.py", &[]);
        let problems = support::check_output(&support::case(other).request(), &files);
        assert!(problems.join("\n").contains(needs), "{problems:?}");
    }
}

#[test]
fn a_query_module_runs_exactly_the_sql_sqlc_wrote() {
    // `invoicing_sqlalchemy` with a planted bug, and `sql` replaced by `sqlc_sql` in the query texts sqlc sent.
    let with_sqlc_text = |file, old, new, (sql, sqlc_sql): (&str, &str)| {
        let (mut request, files) = plant("invoicing_sqlalchemy", file, &[(old, new)]);
        for query in &mut request.queries {
            query.text = query.text.replacen(sql, sqlc_sql, 1);
        }
        support::check_output(&request, &files)
    };
    let same = ("", "");
    for (file, old, new, sqlc, problem) in [
        // The cast glued to the placeholder hides the bind.
        (
            "invoices.py",
            r"ANY(:ids\\:\\:uuid[])",
            "ANY(:ids::uuid[])",
            same,
            "binds",
        ),
        // A literal colon read as a bind.
        ("customers.py", r#""vip\"\\:true"#, r#""vip\":true"#, same, "binds"),
        // A bind that is not the parameter's name.
        ("customers.py", r"WHERE :tag\\:", r"WHERE :tags\\:", same, "binds"),
        // A chained cast left unescaped: SQLAlchemy would send `\:text`.
        (
            "customers.py",
            r"'%' || :query\\:\\:text ||",
            r"'%' || :query\\:\\:text::text ||",
            ("'%' || $1::text", "'%' || $1::text::text"),
            "runs",
        ),
        // SQL that is not sqlc's.
        (
            "customers.py",
            "ORDER BY name\"\"\")",
            "ORDER BY name DESC\"\"\")",
            same,
            "runs",
        ),
        // A query without its statement.
        (
            "payments.py",
            "_LIST_PAYMENTS_BY_METHODS: Final = sqlalchemy.text(",
            "_X = (",
            same,
            "statements",
        ),
    ] {
        let problems = with_sqlc_text(file, old, new, sqlc).join("\n");
        assert!(problems.contains(problem), "{new:?} not caught: {problems}");
    }
    // A `%` PyMySQL would read as a placeholder's start.
    let problems = planted(
        "billing_pymysql",
        "query.py",
        "NOT LIKE '%%:test%%'",
        "NOT LIKE '%:test%'",
    )
    .join("\n");
    assert!(
        problems.contains("SearchCustomers: `%:` is no format PyMySQL reads"),
        "{problems}"
    );
    // The same chain, escaped all along, is what sqlc wrote.
    assert_eq!(
        with_sqlc_text(
            "customers.py",
            r"'%' || :query\\:\\:text ||",
            r"'%' || :query\\:\\:text\\:\\:text ||",
            ("'%' || $1::text", "'%' || $1::text::text"),
        ),
        Vec::<String>::new()
    );
}

#[test]
fn the_files_are_the_models_and_one_module_per_query_file() {
    let file = |name: &str, contents: &[u8]| plugin::File {
        name: name.into(),
        contents: contents.into(),
    };
    for (edit, problem) in [
        // A query file whose queries vanished with their module, and one module too many, which would hold no query to
        // check.
        (
            Box::new(|files: &mut Vec<plugin::File>| files.retain(|f| f.name != "payments.py"))
                as Box<dyn FnOnce(&mut Vec<plugin::File>)>,
            "one module per query file",
        ),
        (
            Box::new(|files: &mut Vec<_>| files.push(file("extra.py", b""))),
            "one module per query file",
        ),
        (
            Box::new(|files: &mut Vec<_>| files.push(file("models.py", b""))),
            "models.py: written twice",
        ),
    ] {
        let problems = files_planted(edit).join("\n");
        assert!(problems.contains(problem), "{problem}: {problems}");
    }
}

#[test]
fn a_module_binds_each_name_once() {
    for (source, problem) in [
        ("import enum\nimport enum\n", "module: `enum` is bound twice"),
        (
            "class A(enum.StrEnum):\n    X = \"x\"\n    X = \"y\"\n",
            "class A: `X` is bound twice",
        ),
    ] {
        let problems = module(source);
        assert!(problems.contains(problem), "{source:?}: {problems}");
    }
}

#[test]
fn the_only_comments_are_the_header() {
    // A `# type: ignore` would silence both type checkers on its line.
    let problems = planted(
        "invoicing_sqlalchemy",
        "payments.py",
        "    return [\n",
        "    return [  # type: ignore[return-value]\n",
    )
    .join("\n");
    assert!(
        problems.contains("`# type: ignore[return-value]` is a comment after the header"),
        "{problems}"
    );
    // Nor in the header, where it would silence ruff for the whole module.
    let problems = planted(
        "invoicing_sqlalchemy",
        "payments.py",
        "# Code generated by sqlc-gen-python. DO NOT EDIT.\n",
        "# Code generated by sqlc-gen-python. DO NOT EDIT.\n# ruff: noqa\n",
    )
    .join("\n");
    assert!(problems.contains("`# ruff: noqa` silences a tool"), "{problems}");
    assert_eq!(module("# a\n# b\nx = 1\n"), "");
}

#[test]
fn every_comment_is_a_docstring_of_what_it_documents_and_nothing_else_is() {
    for (file, old, new, problem) in [
        // An enum's, a table's, a column's and a query's comment dropped.
        (
            "models.py",
            "    \"\"\"Lifecycle of an invoice. Only drafts may be deleted.\"\"\"\n\n",
            "",
            "class `InvoiceStatus`: docstrings",
        ),
        (
            "models.py",
            "    \"\"\"Someone we send invoices to.\"\"\"\n\n",
            "",
            "class `Customer`: docstrings",
        ),
        (
            "models.py",
            "    \"\"\"Printed under the totals.",
            "    \"\"\"Printed above the totals.",
            "class `Invoice`: docstrings",
        ),
        (
            "payments.py",
            "    \"\"\"An unnamed column: PostgreSQL names it `array`, and sqlc nothing.\"\"\"\n",
            "",
            "ListInvoicePaymentMethods: its docstring is not its comment",
        ),
        // One invented.
        (
            "payments.py",
            ") -> list[models.BillingPayment]:\n    return [",
            ") -> list[models.BillingPayment]:\n    \"\"\"Payments.\"\"\"\n    return [",
            "ListPaymentsByMethods: its docstring is not its comment",
        ),
    ] {
        let problems = planted("invoicing_sqlalchemy", file, old, new).join("\n");
        assert!(problems.contains(problem), "{new:?} not caught: {problems}");
    }
}

#[test]
fn every_name_is_the_one_the_catalog_and_sqlc_give() {
    for (file, edits, problem) in [
        // A label the database does not have, and a member not named after its.
        (
            "models.py",
            &[("BANK_TRANSFER = \"bank_transfer\"", "BANK_TRANSFER = \"bank-transfer\"")][..],
            "class `BillingPaymentMethod` has no SQL enum of its values",
        ),
        (
            "models.py",
            &[("    VOID = \"void\"", "    VOID = \"voided\"")],
            "class `InvoiceStatus` has no SQL enum of its values",
        ),
        (
            "models.py",
            &[("BANK_TRANSFER = \"bank_transfer\"", "BANKTRANSFER = \"bank_transfer\"")],
            "are not named after their labels",
        ),
        // Two fields of one type swapped by name, in the class and in `from_row` alike, so every type and position
        // still agrees.
        (
            "models.py",
            &[
                (
                    "    id: UUID\n    invoice_id: UUID\n",
                    "    invoice_id: UUID\n    id: UUID\n",
                ),
                (
                    "id=row[0],\n            invoice_id=row[1],",
                    "invoice_id=row[0],\n            id=row[1],",
                ),
            ],
            "class `BillingPayment` has no table with its fields' names",
        ),
        (
            "invoices.py",
            &[
                ("    vat: Decimal\n", "    tax: Decimal\n"),
                ("vat=row[4]", "tax=row[4]"),
            ],
            "class `GetInvoiceTotalsRow`: [\"id\", \"number\", \"line_count\", \"subtotal\", \"tax\"] are not named after sqlc's",
        ),
        // A parameter renamed in the signature, the dict and the SQL at once.
        (
            "payments.py",
            &[
                (":from_,", ":sender,"),
                ("    from_: str | None = None,\n", "    sender: str | None = None,\n"),
                ("\"from_\": from_,", "\"sender\": sender,"),
            ],
            "RecordPayment: parameters: [",
        ),
    ] {
        let problems = planted_all("invoicing_sqlalchemy", file, edits).join("\n");
        assert!(problems.contains(problem), "{edits:?} not caught: {problems}");
    }
}

#[test]
fn a_function_is_called_safely() {
    for (case, file, old, new, problem) in [
        // The connection made keyword-only.
        (
            "invoicing_sqlalchemy",
            "invoices.py",
            "def get_invoice(connection: sqlalchemy.Connection, *, id: UUID)",
            "def get_invoice(*, connection: sqlalchemy.Connection, id: UUID)",
            "positionally",
        ),
        // A parameter that can be forgotten.
        (
            "invoicing_sqlalchemy",
            "invoices.py",
            "*, id: UUID) -> models.Invoice:",
            "*, id: UUID = UUID(int=0)) -> models.Invoice:",
            "has a default",
        ),
        // A nullable parameter left out as something but NULL.
        (
            "invoicing_sqlalchemy",
            "customers.py",
            "*, email: str | None = None\n) -> UUID:",
            "*, email: str | None = \"\"\n) -> UUID:",
            "has a default",
        ),
        // A coroutine where functions were asked for, and the reverse.
        (
            "invoicing_sqlalchemy",
            "invoices.py",
            "def get_invoice(connection",
            "async def get_invoice(connection",
            "is `async def` exactly when async",
        ),
        (
            "invoicing_sqlalchemy_async",
            "invoices.py",
            "async def get_invoice(\n    connection",
            "def get_invoice(\n    connection",
            "is `async def` exactly when async",
        ),
        // The wrong connection, which the type checkers would pass on.
        (
            "invoicing_psycopg",
            "invoices.py",
            "def get_invoice(connection: psycopg.Connection[Any]",
            "def get_invoice(connection: psycopg.AsyncConnection[Any]",
            "takes a connection",
        ),
    ] {
        let problems = planted(case, file, old, new).join("\n");
        assert!(problems.contains(problem), "{new:?} not caught: {problems}");
    }
}

#[test]
fn a_function_runs_its_own_statement_with_each_parameter_as_itself() {
    for (case, file, old, new, problem) in [
        // The neighbour's statement, which binds the same `id`.
        (
            "invoicing_sqlalchemy",
            "invoices.py",
            "connection.execute(_GET_INVOICE, {\"id\": id}).one()",
            "connection.execute(_GET_INVOICE_DUE_DATE, {\"id\": id}).one()",
            "not its own statement",
        ),
        (
            "ledger_sqlite3",
            "query.py",
            "cur.execute(_GET_ACCOUNT, (id,))",
            "cur.execute(_IS_ARCHIVED, (id,))",
            "not its own statement",
        ),
        // Two values of one type swapped, which type-checks.
        (
            "invoicing_sqlalchemy",
            "invoices.py",
            "\"issued_on\": issued_on,\n                \"due_on\": due_on,",
            "\"issued_on\": due_on,\n                \"due_on\": issued_on,",
            "not each parameter as itself",
        ),
        (
            "invoicing_sqlalchemy",
            "line_items.py",
            "\"quantity\": r.quantity,\n            \"unit_price\": r.unit_price,",
            "\"quantity\": r.unit_price,\n            \"unit_price\": r.quantity,",
            "not each parameter as itself",
        ),
        // psycopg's `COPY` writes each row's values in its columns' order.
        (
            "invoicing_psycopg",
            "line_items.py",
            "r.quantity,\n                        r.unit_price,",
            "r.unit_price,\n                        r.quantity,",
            "not each parameter as itself",
        ),
        (
            "invoicing_psycopg",
            "invoices.py",
            "_CREATE_INVOICE, (customer_id, number, currency, issued_on, due_on, notes)",
            "_CREATE_INVOICE, (customer_id, number, currency, due_on, issued_on, notes)",
            "not each parameter as itself",
        ),
        // A slice bound as one value, and a parameter used twice bound once.
        (
            "billing_pymysql",
            "query.py",
            "(*statuses, customer_id)",
            "(statuses, customer_id)",
            "not each parameter as itself",
        ),
        // A slice's items, then the parameter after it: by number, `?2` would bind the slice's second item.
        (
            "ledger_sqlite3",
            "query.py",
            "AND account_id = ?\n",
            "AND account_id = ?2\n",
            "not sqlc's",
        ),
        (
            "billing_pymysql",
            "query.py",
            "_SEARCH_CUSTOMERS, (pattern, pattern)",
            "_SEARCH_CUSTOMERS, (pattern,)",
            "not each parameter as itself",
        ),
        // The caller's transaction ended under it, or another statement run.
        (
            "invoicing_sqlalchemy",
            "payments.py",
            "    return models.BillingPayment.from_row(\n",
            "    connection.commit()\n    return models.BillingPayment.from_row(\n",
            "reads `connection` 2 times",
        ),
        (
            "invoicing_sqlalchemy",
            "invoices.py",
            "    return models.Invoice.from_row(connection.execute(_GET_INVOICE",
            "    connection.exec_driver_sql(\"SELECT 1\")\n    return models.Invoice.from_row(connection.execute(_GET_INVOICE",
            "reads `connection` 2 times",
        ),
        // A coroutine never awaited never runs.
        (
            "invoicing_sqlalchemy_async",
            "line_items.py",
            "for row in await connection.execute(",
            "for row in connection.execute(",
            "awaits its statement exactly when async",
        ),
    ] {
        let problems = planted(case, file, old, new).join("\n");
        assert!(problems.contains(problem), "{new:?} not caught: {problems}");
    }
}

#[test]
fn a_record_reads_each_field_from_its_own_place_in_the_row() {
    for (file, old, new) in [
        // Two values of one type swapped, in a model and in a query's row.
        (
            "models.py",
            "id=row[0],\n            invoice_id=row[1],",
            "id=row[1],\n            invoice_id=row[0],",
        ),
        (
            "invoices.py",
            "subtotal=row[3], vat=row[4]",
            "subtotal=row[4], vat=row[3]",
        ),
        // An embed that starts a column late.
        ("invoices.py", "row[10:18]", "row[11:18]"),
    ] {
        let problems = planted("invoicing_sqlalchemy", file, old, new).join("\n");
        assert!(
            problems.contains("`from_row` reads row["),
            "{new:?} not caught: {problems}"
        );
    }
}

#[test]
fn a_model_is_returned_exactly_when_the_result_is_its_tables_columns() {
    let problems = planted(
        "invoicing_sqlalchemy",
        "invoices.py",
        "def get_invoice(connection: sqlalchemy.Connection, *, id: UUID) -> models.Invoice:",
        "def get_invoice(connection: sqlalchemy.Connection, *, id: UUID) -> GetInvoiceWithCustomerRow:",
    )
    .join("\n");
    assert!(problems.contains("a model is returned exactly when"), "{problems}");
}

#[test]
fn a_record_is_a_frozen_slotted_keyword_only_dataclass_without_defaults() {
    let record = "@dataclasses.dataclass(frozen=True, slots=True, kw_only=True)\nclass Customer:";
    for (file, old, new, problem) in [
        // A default lets an omitted value through as NULL: no type error.
        (
            "models.py",
            "    email: str | None\n",
            "    email: str | None = None\n",
            "`Customer.email` has a default",
        ),
        (
            "line_items.py",
            "    vat_rate: Decimal\n",
            "    vat_rate: Decimal = Decimal(0)\n",
            "`CreateLineItemsParams.vat_rate` has a default",
        ),
        // Positional construction, mutation, or no dataclass at all.
        (
            "models.py",
            record,
            "@dataclasses.dataclass(frozen=True, slots=True)\nclass Customer:",
            "`Customer` is not exactly",
        ),
        (
            "models.py",
            record,
            "@dataclasses.dataclass(slots=True, kw_only=True)\nclass Customer:",
            "`Customer` is not exactly",
        ),
        ("models.py", record, "class Customer:", "`Customer` is not exactly"),
    ] {
        let problems = planted("invoicing_sqlalchemy", file, old, new).join("\n");
        assert!(problems.contains(problem), "{new:?} not caught: {problems}");
    }
}

#[test]
fn a_pydantic_record_is_frozen_forbids_extra_fields_and_is_of_the_one_kind_asked_for() {
    for (old, new) in [
        // A record pydantic lets change, or that silently drops a misspelt field of a `**` dict
        ("frozen=True, extra=", "frozen=False, extra="),
        ("extra=\"forbid\", ", ""),
        // The other kind of record, which the options did not ask for
        (
            "class Oddity(pydantic.BaseModel):\n    model_config = pydantic.ConfigDict(\n        frozen=True, extra=\"forbid\", arbitrary_types_allowed=True\n    )\n",
            "@dataclasses.dataclass(frozen=True, slots=True, kw_only=True)\nclass Oddity:\n",
        ),
    ] {
        let problems = planted("pgtypes_pydantic", "models.py", old, new).join("\n");
        assert!(
            problems.contains("is not exactly a `pydantic.BaseModel`"),
            "{new:?} not caught: {problems}"
        );
    }
}

#[test]
fn a_result_is_read_and_shaped_as_its_command_returns_it() {
    for (file, old, new, problem) in [
        // A `:one` that would take the first of several rows
        (
            "invoices.py",
            "connection.execute(_GET_INVOICE, {\"id\": id}).one()",
            "connection.execute(_GET_INVOICE, {\"id\": id}).first()",
            ":one reads its row with none of",
        ),
        (
            "line_items.py",
            "-> list[models.LineItem]:",
            "-> tuple[models.LineItem, ...]:",
            ":many returns `tuple",
        ),
        (
            "line_items.py",
            "rows: Iterable[CreateLineItemsParams]\n) -> int:",
            "rows: Iterable[CreateLineItemsParams]\n) -> None:",
            ":copyfrom returns `None`",
        ),
        ("models.py", "    vat_rate: Decimal\n", "", "6 fields for 7 columns"),
    ] {
        let problems = planted("invoicing_sqlalchemy", file, old, new).join("\n");
        assert!(problems.contains(problem), "{new:?} not caught: {problems}");
    }
}

#[test]
fn every_table_and_enum_has_its_class_with_a_from_row() {
    for (file, old, new, problem) in [
        (
            "models.py",
            "    DRAFT = \"draft\"",
            "    DRAFT = \"drafted\"",
            "enum `invoice_status` has no class",
        ),
        (
            "models.py",
            "    vat_rate: Decimal\n",
            "",
            "table `line_items` has no class",
        ),
        (
            "invoices.py",
            "    def from_row(cls, row: Sequence[Any]) -> \"GetInvoiceWithCustomerRow\":",
            "    def from_rows(cls, row: Sequence[Any]) -> \"GetInvoiceWithCustomerRow\":",
            "class `GetInvoiceWithCustomerRow`: no `from_row` to build it",
        ),
    ] {
        let problems = planted("invoicing_sqlalchemy", file, old, new).join("\n");
        assert!(problems.contains(problem), "{new:?} not caught: {problems}");
    }
}

#[test]
fn a_stream_takes_what_its_function_takes_and_yields_its_rows_one_by_one() {
    let stream = "@contextlib.contextmanager
def stream_list_line_items(
    connection: sqlalchemy.Connection, *, invoice_id: UUID
) -> Generator[Iterator[models.LineItem], None, None]:
    with connection.execute(
        _LIST_LINE_ITEMS,
        {\"invoice_id\": invoice_id},
        execution_options={\"stream_results\": True},
    ) as result:
        yield (models.LineItem.from_row(row) for row in result)
";
    for (old, new, problem) in [
        (
            stream,
            "",
            "ListLineItems: streams exactly when it is a :many that only reads",
        ),
        (
            "def stream_list_line_items(",
            "def stream_line_items(",
            "is not named after its function",
        ),
        (
            "@contextlib.contextmanager\ndef stream_list_line_items",
            "@contextlib.asynccontextmanager\ndef stream_list_line_items",
            "is not one contextlib.contextmanager",
        ),
        (
            "invoice_id: UUID\n) -> Generator",
            "invoice_id: UUID | None\n) -> Generator",
            "does not take what its function takes",
        ),
        (
            "Generator[Iterator[models.LineItem], None, None]",
            "Generator[Iterator[models.LineItem | None], None, None]",
            "does not yield an iterator of the rows its function returns a list of",
        ),
        // Every row read before the first is yielded
        (
            "yield (models.LineItem.from_row(row) for row in result)",
            "yield iter([models.LineItem.from_row(row) for row in result])",
            "does not yield, once, a generator of its cursor's rows",
        ),
    ] {
        let problems = planted("invoicing_sqlalchemy", "line_items.py", old, new).join("\n");
        assert!(problems.contains(problem), "{new:?} not caught: {problems}");
    }
}
