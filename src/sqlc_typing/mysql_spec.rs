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
fn a_set_is_told_from_an_enum_by_its_length() {
    let labels = ["one".to_string(), "two".into(), "three".into()];
    assert!(set_length(&column("t_cset", 13), &labels).unwrap());
    assert!(!set_length(&column("t_cenum", 5), &labels).unwrap());
}

#[test]
fn a_set_is_measured_in_bytes_and_one_member_is_ambiguous() {
    let labels = ["é".to_string(), "ü".into()];
    // Each label is two bytes, so a SET's length is 2 + 1 + 2
    assert!(set_length(&column("t_cset", 5), &labels).unwrap());
    assert!(!set_length(&column("t_cenum", 2), &labels).unwrap());
    assert!(set_length(&column("t_missing", -1), &labels).is_err());
    let error = set_length(&column("t_one", 1), &["a".to_string()]).unwrap_err();
    assert!(error.contains("one member"), "{error}");
}

#[test]
fn only_a_missing_length_can_come_from_the_source_column_of_the_same_type() {
    use crate::api::fixtures::{public, table};

    let labels = ["a".into(), "bb".into(), "c".into()];
    let source = plugin::Column {
        name: "flags".into(),
        ..column("sample_flags", 6)
    };
    let table = table("sample", vec![source]);
    let reported = plugin::Column {
        original_name: "flags".into(),
        table: table.rel.clone(),
        ..column("sample_flags", -1)
    };
    let catalog = plugin::Catalog {
        default_schema: "public".into(),
        schemas: public(vec![table]),
        ..Default::default()
    };
    let is_set = |c: &plugin::Column| is_set_column(Engine::MySql, &catalog, c, &labels);
    assert!(is_set(&reported).unwrap());
    assert!(
        is_set(&plugin::Column {
            length: 3,
            ..reported.clone()
        })
        .is_err()
    );
    assert!(
        is_set(&plugin::Column {
            r#type: column("other", -1).r#type,
            ..reported
        })
        .is_err()
    );
}
