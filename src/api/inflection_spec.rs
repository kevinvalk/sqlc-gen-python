use super::*;

#[test]
fn every_name_singularises_as_sqlc_singularises_it() {
    // `name<TAB>sqlc's singular`, from sqlc's own code (`cargo xtask capture`).
    let table = include_str!("../../tests/sqlc/endtoend/inflection.tsv");
    let wrong: Vec<String> = (table.lines())
        .filter_map(|line| line.split_once('\t'))
        .filter(|(name, sqlc)| singular(name) != *sqlc)
        .map(|(name, sqlc)| format!("{name}: {} (sqlc: {sqlc})", singular(name)))
        .collect();
    assert!(table.lines().count() > 100, "inflection.tsv is too short");
    assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
}

#[test]
fn the_case_of_the_name_is_kept() {
    assert_eq!(singular("People"), "Person");
    assert_eq!(singular("AUTHORS"), "AUTHOR");
    assert_eq!(singular("Authors"), "Author");
    // Of one of sqlc's corrections too.
    assert_eq!(singular("Campus"), "Campus");
}
