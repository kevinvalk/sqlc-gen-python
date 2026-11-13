//! Specification for reading what each result column of a statement is.

use super::*;

/// The calls of `sql`'s result columns, split into tokens at spaces: each
/// call's function, `None` for anything else.
fn of(sql: &str, columns: usize) -> Option<Vec<Option<String>>> {
    let tokens: Vec<String> = sql.split_whitespace().map(String::from).collect();
    let items = items(&tokens, columns)?;
    Some(
        (items.into_iter())
            .map(|item| match item {
                Item::Call(name) => Some(name),
                _ => None,
            })
            .collect(),
    )
}

fn names(calls: &[Option<&str>]) -> Option<Vec<Option<String>>> {
    Some(calls.iter().map(|c| c.map(String::from)).collect())
}

#[test]
fn each_item_of_the_select_list_is_a_bare_call_or_nothing() {
    assert_eq!(
        of(
            "SELECT id , COUNT ( * ) , count ( x ) AS n , sum ( y ) total FROM t",
            4
        ),
        names(&[None, Some("count"), Some("count"), Some("sum")])
    );
    assert_eq!(
        of("SELECT DISTINCT ON ( a ) a , count ( * ) FROM t", 2),
        names(&[None, Some("count")])
    );
    // The main statement's list, after a CTE's own.
    assert_eq!(
        of(
            "WITH c AS ( SELECT sum ( x ) FROM t ) SELECT count ( * ) FROM c",
            1
        ),
        names(&[Some("count")])
    );
    assert_eq!(
        of("INSERT INTO t ( a ) VALUES ( 1 ) RETURNING id , now ( )", 2),
        names(&[None, Some("now")])
    );
}

#[test]
fn a_call_with_more_after_it_is_no_bare_call() {
    for sql in [
        "SELECT count ( * ) + 1 FROM t",
        "SELECT count ( * ) :: int FROM t",
        "SELECT count ( * ) OVER ( ) FROM t",
        "SELECT count ( * ) FILTER ( WHERE x ) FROM t",
        "SELECT \"count\" ( x ) FROM t",
    ] {
        assert_eq!(of(sql, 1), names(&[None]), "{sql}");
    }
}

#[test]
fn a_list_that_does_not_match_the_columns_one_to_one_has_no_answer() {
    assert_eq!(
        of("SELECT count ( * ) FROM a UNION SELECT 1 FROM b", 1),
        None
    );
    assert_eq!(of("SELECT a , count ( * ) FROM t", 3), None);
    assert_eq!(of("UPDATE t SET a = 1", 1), None);
}

#[test]
fn a_left_joined_table_may_have_no_row_and_after_a_right_join_any_may() {
    let joined = |sql: &str| {
        let tokens: Vec<String> = sql.split_whitespace().map(String::from).collect();
        outer_joined(&tokens)
    };
    assert_eq!(
        joined(
            "SELECT * FROM a JOIN b ON x LEFT OUTER JOIN public . c ON y LEFT JOIN \"D\" d ON z"
        ),
        Some(vec!["c".to_string(), "d".to_string()])
    );
    // A subquery's own joins are its own.
    assert_eq!(
        joined("SELECT * FROM a JOIN ( SELECT * FROM b LEFT JOIN c ) s"),
        Some(vec![])
    );
    assert_eq!(joined("SELECT * FROM a RIGHT JOIN b ON x"), None);
    assert_eq!(joined("SELECT * FROM a FULL OUTER JOIN b ON x"), None);
}

#[test]
fn a_scalar_subquery_has_no_row_unless_it_counts_ungrouped() {
    let subqueries = |sql: &str| {
        let tokens: Vec<String> = sql.split_whitespace().map(String::from).collect();
        items(&tokens, 1).unwrap()
    };
    let row = |row| vec![Item::Subquery { row }];
    assert_eq!(
        subqueries("SELECT ( SELECT a FROM t LIMIT 1 ) AS a"),
        row(false)
    );
    assert_eq!(
        subqueries("SELECT ( SELECT count ( * ) FROM t ) n"),
        row(true)
    );
    assert_eq!(
        subqueries("SELECT ( SELECT count ( * ) FROM t GROUP BY a ) n"),
        row(false)
    );
    assert_eq!(
        subqueries("SELECT ( SELECT a FROM t ) + 1"),
        vec![Item::Other]
    );
}
