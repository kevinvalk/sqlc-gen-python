use super::*;

fn column(name: &str, length: i32) -> plugin::Column {
    plugin::Column {
        r#type: Some(plugin::Identifier {
            name: name.into(),
            ..Default::default()
        }),
        length,
        ..Default::default()
    }
}

#[test]
fn a_type_maps_to_what_pymysql_and_asyncmy_return() {
    for (name, length, python) in [
        ("int", -1, Type::INT),
        ("bigint unsigned", -1, Type::INT),
        ("tinyint", -1, Type::INT),
        ("year", -1, Type::INT),
        ("double", -1, Type::FLOAT),
        ("decimal", 10, Type::DECIMAL),
        ("varchar", 255, Type::STR),
        ("longtext", -1, Type::STR),
        ("json", -1, Type::STR),
        ("varbinary", 32, Type::BYTES),
        ("bit", 1, Type::BYTES),
        ("date", -1, Type::DATE),
        ("datetime", 19, Type::DATETIME),
        ("timestamp", 19, Type::DATETIME),
        ("time", 10, Type::TIMEDELTA),
    ] {
        assert_eq!(scalar_type(&column(name, length)), Some(python), "{name}");
    }
}

#[test]
fn a_boolean_is_tinyint_1_converted() {
    for (name, length) in [("bool", -1), ("tinyint", 1)] {
        assert_eq!(scalar_type(&column(name, length)), Some(Type::BOOL_FROM_INT), "{name}");
    }
}

#[test]
fn a_type_nobody_probed_is_not_mapped() {
    // sqlc reports a `CAST(... AS CHAR)` and a `CAST(... AS BINARY)` alike, as `var_string`: text of one, bytes of the
    // other
    for name in ["geometry", "point", "var_string", "any", ""] {
        assert_eq!(scalar_type(&column(name, -1)), None, "{name}");
    }
}

/// The runtime proof (`tests/python/typemap_mysql.py`) reads this fixture back through every driver.
#[test]
fn every_mapped_type_has_a_column_the_runtime_proof_reads() {
    // We read from where cargo runs the test and not where it was built, as a reused `target/` may come from another
    // copy of the tree
    let path = std::path::Path::new(&std::env::var_os("CARGO_MANIFEST_DIR").unwrap())
        .join("tests/sqlc/examples/mysqltypes.json");
    let request: plugin::GenerateRequest = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    // What the queries return and take counts too, as a comparison's `bool` and a `LIMIT`'s `integer` are no column's
    // type
    let tables = (request.catalog.iter())
        .flat_map(|catalog| &catalog.schemas)
        .flat_map(|schema| &schema.tables)
        .flat_map(|table| &table.columns);
    let results = request.queries.iter().flat_map(|query| &query.columns);
    let parameters = (request.queries.iter())
        .flat_map(|query| &query.params)
        .filter_map(|parameter| parameter.column.as_ref());
    let probed: Vec<&plugin::Column> = tables.chain(results).chain(parameters).collect();

    let reads = |name: &str| probed.iter().any(|column| column.data_type() == name);
    let missing: Vec<&str> = (TYPES.iter().map(|(name, _)| *name))
        .filter(|name| !reads(name))
        .collect();
    assert!(missing.is_empty(), "no column of {missing:?}");
    let boolean = |column: &&plugin::Column| scalar_type(column) == Some(Type::BOOL_FROM_INT);
    assert!(probed.iter().any(|c| c.data_type() == "tinyint" && boolean(c)));
    assert!(probed.iter().any(|c| c.data_type() == "tinyint" && !boolean(c)));
}

#[test]
fn a_set_is_told_from_an_enum_by_its_length() {
    let labels = ["one".to_string(), "two".into(), "three".into()];
    assert!(enum_problem(&column("t_cset", 13), &labels).is_some());
    assert!(enum_problem(&column("t_cenum", 5), &labels).is_none());
}

#[test]
fn a_set_is_measured_in_bytes_and_one_member_is_ambiguous() {
    let labels = ["é".to_string(), "ü".into()];
    // Each label is two bytes, so a SET's length is 2 + 1 + 2
    assert!(enum_problem(&column("t_cset", 5), &labels).is_some());
    assert!(enum_problem(&column("t_cenum", 2), &labels).is_none());
    let error = enum_problem(&column("t_one", 1), &["a".to_string()]).unwrap();
    assert!(error.contains("one member"), "{error}");
}
