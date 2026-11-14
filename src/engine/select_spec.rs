//! Specification for reading what sqlc's report of a statement's result is
//! not enough for. Each statement is written with a space between tokens.

use super::*;

/// An engine where an aggregate is one whatever its arguments, and no column
/// stands beside one ungrouped: PostgreSQL.
const AGGREGATES: Aggregates = Aggregates {
    nullable: &["sum", "avg", "min", "max", "string_agg"],
    never_null: &["count", "total"],
    scalar_of_several: &[],
    bare_columns: false,
};

/// SQLite.
const SQLITE: Aggregates = Aggregates {
    scalar_of_several: &["min", "max"],
    bare_columns: true,
    ..AGGREGATES
};

fn tokens(sql: &str) -> Vec<String> {
    sql.split_whitespace().map(String::from).collect()
}

fn read(sql: &str, columns: usize) -> Option<Vec<Item>> {
    items(&tokens(sql), columns, &AGGREGATES)
}

/// Whether each item of `sql`'s list is a count.
fn counts(sql: &str, columns: usize) -> Vec<bool> {
    let items = read(sql, columns).expect(sql);
    items.into_iter().map(|item| item.count).collect()
}

/// Whether each item of `sql`'s list is NULL whatever sqlc reports.
fn nullable(sql: &str, columns: usize) -> Vec<bool> {
    let items = read(sql, columns).expect(sql);
    items.into_iter().map(|item| item.nullable).collect()
}

// -- The result list ----------------------------------------------------------

#[test]
fn an_item_is_a_count_where_it_is_a_bare_call_of_it() {
    assert_eq!(
        counts(
            "SELECT id , COUNT ( * ) , count ( x ) AS n , sum ( y ) total FROM t",
            4
        ),
        [false, true, true, false]
    );
    assert_eq!(
        counts("SELECT DISTINCT ON ( a ) a , count ( * ) FROM t", 2),
        [false, true]
    );
    // The main statement's list, after a CTE's own.
    assert_eq!(
        counts(
            "WITH c AS ( SELECT sum ( x ) FROM t ) SELECT count ( * ) FROM c",
            1
        ),
        [true]
    );
    assert_eq!(
        counts(
            "INSERT INTO t ( a ) VALUES ( 1 ) RETURNING id , count ( * )",
            2
        ),
        [false, true]
    );
    // And behind a scalar subquery's parentheses, alone.
    assert_eq!(counts("SELECT ( SELECT count ( * ) FROM t ) n", 1), [true]);
}

#[test]
fn a_call_with_more_after_it_is_no_count() {
    for sql in [
        "SELECT count ( * ) + 1 FROM t",
        "SELECT count ( * ) :: int FROM t",
        "SELECT count ( * ) OVER ( ) FROM t",
        "SELECT count ( * ) FILTER ( WHERE x ) FROM t",
        "SELECT \"count\" ( x ) FROM t",
        "SELECT ( SELECT count ( * ) FROM t ) + 1 FROM t",
        "SELECT ( SELECT count ( * ) , 1 FROM t ) FROM t",
    ] {
        assert_eq!(counts(sql, 1), [false], "{sql}");
    }
}

#[test]
fn a_list_that_is_not_an_item_per_column_is_not_read() {
    assert_eq!(read("SELECT a , count ( * ) FROM t", 3), None);
    assert_eq!(read("UPDATE t SET a = 1", 1), None);
    // One branch of a set operation is enough, as one without a list.
    assert_eq!(read("SELECT a FROM t UNION SELECT a , b FROM u", 1), None);
    assert_eq!(
        read(
            "SELECT ( SELECT a FROM t LIMIT 1 ) UNION ALL VALUES ( 'x' )",
            1
        ),
        None
    );
}

#[test]
fn a_word_after_a_dot_or_an_as_is_a_name_and_ends_no_clause() {
    for (sql, expected) in [
        (
            "SELECT max ( k . x ) :: int AS m FROM k WHERE k . group = $1",
            &[true][..],
        ),
        ("SELECT max ( k . x ) :: int AS group FROM k", &[true]),
        (
            "SELECT k . order , ( SELECT a FROM t LIMIT 1 ) AS n FROM k",
            &[false, true],
        ),
        (
            "SELECT k . id AS limit , ( SELECT a FROM t LIMIT 1 ) AS n FROM k",
            &[false, true],
        ),
        (
            "SELECT ( SELECT a FROM t LIMIT 1 ) AS within FROM k",
            &[true],
        ),
    ] {
        assert_eq!(nullable(sql, expected.len()), expected, "{sql}");
    }
}

#[test]
fn a_bracket_holds_its_commas_as_a_parenthesis_does() {
    assert_eq!(
        nullable(
            "SELECT ARRAY [ 1 , 2 ] :: int [ ] AS pair , max ( x ) :: int AS m FROM k",
            2
        ),
        [false, true]
    );
}

#[test]
fn is_distinct_from_and_within_group_end_no_list() {
    assert_eq!(
        nullable(
            "SELECT a IS DISTINCT FROM 'x' AS d , ( SELECT b FROM u LIMIT 1 ) AS s FROM t",
            2
        ),
        [false, true]
    );
    // Nor does `WITHIN GROUP` group the statement's rows.
    assert_eq!(
        nullable(
            "SELECT count ( * ) WITHIN GROUP ( ORDER BY a ) , sum ( b ) :: int FROM t",
            2
        ),
        [false, true]
    );
}

#[test]
fn a_set_operation_is_read_branch_by_branch() {
    assert_eq!(
        counts(
            "SELECT count ( * ) FROM a UNION ALL SELECT count ( * ) FROM b",
            1
        ),
        [true]
    );
    assert_eq!(
        counts("SELECT count ( * ) FROM a UNION SELECT 1 FROM b", 1),
        [false]
    );
    // NULL in any branch is NULL in the result, a branch in parentheses too.
    for sql in [
        "SELECT a FROM t UNION ALL SELECT ( SELECT b FROM u LIMIT 1 ) FROM t",
        "( SELECT ( SELECT b FROM u LIMIT 1 ) FROM t ) UNION ALL ( SELECT a FROM t ) ORDER BY 1",
    ] {
        assert_eq!(nullable(sql, 1), [true], "{sql}");
    }
    // What an `INSERT` selects is no branch of it: its list is what it
    // returns.
    assert_eq!(
        nullable(
            "INSERT INTO a ( x ) SELECT x FROM b UNION ALL SELECT x FROM c \
             RETURNING id , ( SELECT x FROM c LIMIT 1 ) AS n",
            2
        ),
        [false, true]
    );
}

#[test]
fn parentheses_and_casts_are_read_through_however_deep() {
    let depth = 1_000;
    let sql = format!(
        "{}SELECT CAST ( ( SELECT a FROM t LIMIT 1 ) AS int ) AS a{}",
        "( ".repeat(depth),
        " )".repeat(depth)
    );
    assert_eq!(nullable(&sql, 1), [true]);
    let casts = format!(
        "SELECT {}( SELECT a FROM t LIMIT 1 ){} AS a",
        "CAST ( ".repeat(depth),
        " AS int )".repeat(depth)
    );
    assert_eq!(nullable(&casts, 1), [true]);
}

// -- What is NULL though sqlc reports it NOT NULL -----------------------------

#[test]
fn a_scalar_subquery_is_null_without_a_row_unless_it_counts_them_all() {
    for sql in [
        "SELECT ( SELECT a FROM t LIMIT 1 ) AS a",
        "SELECT ( SELECT a FROM t LIMIT 1 ) :: int AS a",
        "SELECT CAST ( ( SELECT a FROM t LIMIT 1 ) AS INTEGER ) AS a",
        "SELECT ( ( SELECT a FROM t LIMIT 1 ) ) AS a",
        "SELECT ( WITH q AS ( SELECT 1 ) SELECT a FROM t LIMIT 1 ) AS a",
        "SELECT ( SELECT a FROM t LIMIT 1 ) AS 'a'",
        "SELECT ( SELECT max ( a ) FROM t ) AS a",
        // What groups, filters or limits a count may leave it no row.
        "SELECT ( SELECT count ( * ) FROM t GROUP BY a ) n",
        "SELECT ( SELECT count ( * ) FROM t HAVING count ( * ) > 1 ) n",
        "SELECT ( SELECT count ( * ) FROM t OFFSET 1 ) n",
        "SELECT ( ( SELECT count ( * ) FROM t ) LIMIT 0 ) n",
        // Wherever it stands in the item.
        "SELECT ( SELECT a FROM t LIMIT 1 ) + 1 AS a",
        "SELECT 1 + ( SELECT a FROM t LIMIT 1 ) AS a",
        "SELECT f ( ( SELECT max ( x ) FROM u ) ) FROM t",
    ] {
        assert_eq!(nullable(sql, 1), [true], "{sql}");
    }
    for sql in [
        "SELECT ( SELECT count ( * ) FROM t ) n",
        "SELECT ( SELECT count ( * ) FROM t ) > 0 AS any",
        // One that is no value: a test of its rows, or their array.
        "SELECT EXISTS ( SELECT a FROM t ) AS found",
        "SELECT a IN ( SELECT b FROM u ) FROM t",
        "SELECT a = ANY ( SELECT b FROM u ) FROM t",
        "SELECT ARRAY ( SELECT b FROM u ) FROM t",
        "SELECT ( SELECT a FROM t LIMIT 1 ) IS NULL AS none",
    ] {
        assert_eq!(nullable(sql, 1), [false], "{sql}");
    }
}

#[test]
fn an_ungrouped_aggregate_but_a_count_is_null_of_no_rows() {
    assert_eq!(nullable("SELECT sum ( x ) FROM t", 1), [true]);
    assert_eq!(
        nullable(
            "SELECT max ( x ) :: int , count ( * ) , CAST ( min ( x ) AS INTEGER ) , \
             total ( x ) , sum ( x ) + 1 , \"max\" ( x ) , max ( x ORDER BY a , b ) FROM t",
            7
        ),
        [true, false, true, false, true, true, true]
    );
    // A group has a row, and a window function is of each row: sqlc's report
    // stands.
    for sql in [
        "SELECT sum ( x ) :: int FROM t GROUP BY a",
        "SELECT sum ( x ) OVER ( ) FROM t",
        "SELECT sum ( x ) FILTER ( WHERE a ) OVER ( ) FROM t",
    ] {
        assert_eq!(nullable(sql, 1), [false], "{sql}");
    }
    // What groups no rows into a row leaves none: the empty grouping set, a
    // `HAVING` or a `WINDOW` alone; nor does another call's `OVER`, or a
    // column's name.
    for sql in [
        "SELECT sum ( x ) :: int FROM t GROUP BY ( )",
        "SELECT sum ( x ) :: int FROM t GROUP BY ROLLUP ( a )",
        "SELECT sum ( x ) :: int FROM t GROUP BY GROUPING SETS ( ( a ) , ( ) )",
        "SELECT sum ( x ) :: int FROM t HAVING count ( * ) < 10",
        "SELECT sum ( x ) :: int FROM t WINDOW w AS ( PARTITION BY a )",
        "SELECT ( sum ( x ) + count ( * ) OVER ( ) ) :: int FROM t",
        "SELECT sum ( t . over ) :: int FROM t",
    ] {
        assert_eq!(nullable(sql, 1), [true], "{sql}");
    }
}

#[test]
fn a_coalesce_guards_only_with_a_default() {
    for sql in [
        "SELECT coalesce ( sum ( x ) , 0 ) :: int FROM t",
        "SELECT coalesce ( sum ( x ) , sum ( y ) , 0 :: numeric ( 12 , 2 ) ) FROM t",
        "SELECT coalesce ( max ( x ) , - 1 ) FROM t",
        "SELECT ifnull ( max ( x ) , '' ) FROM t",
        "SELECT coalesce ( ( SELECT a FROM u LIMIT 1 ) , false ) FROM t",
    ] {
        assert_eq!(nullable(sql, 1), [false], "{sql}");
    }
    for sql in [
        "SELECT coalesce ( max ( x ) , max ( y ) ) :: int FROM t",
        "SELECT coalesce ( max ( x ) ) FROM t",
        "SELECT coalesce ( max ( x ) , 0 + max ( y ) ) FROM t",
        "SELECT coalesce ( max ( x ) , 0 :: int OR max ( y ) ) FROM t",
        "SELECT coalesce ( ( SELECT a FROM u LIMIT 1 ) , t . b ) FROM t",
    ] {
        assert_eq!(nullable(sql, 1), [true], "{sql}");
    }
}

#[test]
fn a_column_beside_an_ungrouped_aggregate_is_null_of_no_rows_where_one_may_stand() {
    let read = |sql: &str, aggregates: &Aggregates| -> Vec<bool> {
        let items = items(&tokens(sql), 3, aggregates).expect(sql);
        items.into_iter().map(|item| item.nullable).collect()
    };
    let bare = "SELECT id , CAST ( max ( x ) AS INTEGER ) AS m , count ( * ) FROM k";
    assert_eq!(read(bare, &SQLITE), [true, true, false]);
    assert_eq!(read(bare, &AGGREGATES), [false, true, false]);
    let grouped = "SELECT id , CAST ( max ( x ) AS INTEGER ) AS m , count ( * ) FROM k GROUP BY id";
    assert_eq!(read(grouped, &SQLITE), [false, false, false]);
    // SQLite's `max` of several arguments is no aggregate, but of each row.
    let scalar = "SELECT id , max ( a , b ) , min ( a , b ) FROM k";
    assert_eq!(read(scalar, &SQLITE), [false, false, false]);
}

// -- One row, whatever the data -----------------------------------------------

fn single(sql: &str) -> bool {
    single_row(&tokens(sql), &AGGREGATES)
}

#[test]
fn a_select_that_aggregates_its_rows_returns_exactly_one() {
    for sql in [
        "SELECT count ( * ) FROM t WHERE a = $1",
        "SELECT coalesce ( sum ( x ) , 0 ) :: numeric , max ( y ) FROM t",
        "SELECT CAST ( coalesce ( sum ( x ) , 0 ) AS INTEGER ) FROM t",
        "SELECT 1 + count ( * ) FROM t",
        "SELECT count ( * ) FILTER ( WHERE a ) FROM t",
        "SELECT max ( coalesce ( a , b ) ) FROM t",
        "WITH v AS ( SELECT a FROM t ) SELECT count ( * ) FROM v",
        "( SELECT count ( * ) FROM t )",
    ] {
        assert!(single(sql), "{sql}");
    }
    for sql in [
        "SELECT count ( * ) FROM t GROUP BY a",
        "SELECT count ( * ) FROM t WHERE t . within GROUP BY t . y",
        "SELECT count ( * ) FROM t HAVING count ( * ) > 1",
        "SELECT count ( * ) FROM t LIMIT 0",
        "( SELECT count ( * ) FROM t ) LIMIT 0",
        "SELECT count ( * ) OVER ( ) FROM t",
        "SELECT coalesce ( max ( a ) OVER ( ) , 0 ) :: int FROM t",
        "SELECT a FROM t",
        "SELECT 1",
        "SELECT ( SELECT count ( * ) FROM u ) FROM t",
        "SELECT count ( * ) FROM a UNION ALL SELECT count ( * ) FROM b",
        "UPDATE t SET a = 1 RETURNING ( SELECT count ( * ) FROM t )",
    ] {
        assert!(!single(sql), "{sql}");
    }
    // SQLite's scalar `max`, of each row.
    for sql in [
        "SELECT max ( a , b ) FROM t",
        "SELECT coalesce ( min ( a , b ) , 0 ) FROM t",
    ] {
        assert!(!single_row(&tokens(sql), &SQLITE), "{sql}");
    }
    // A schema's own function of an aggregate's name is none.
    assert!(!single("SELECT app . max ( a , b ) FROM t"));
    assert!(single("SELECT pg_catalog . max ( a ) FROM t"));
}

#[test]
fn exists_is_one_row_only_of_no_table_and_unfiltered() {
    assert!(single("SELECT EXISTS ( SELECT 1 FROM t ) AS found"));
    assert!(single("SELECT NOT EXISTS ( SELECT 1 FROM t )"));
    // Of each row of `t`, which may be none.
    assert!(!single(
        "SELECT EXISTS ( SELECT 1 FROM u WHERE u . t = t . id ) FROM t WHERE t . id = $1"
    ));
    assert!(!single("SELECT EXISTS ( SELECT 1 FROM t ) WHERE $1"));
}

#[test]
fn an_insert_of_one_row_of_values_returns_it_unless_something_may_skip_it() {
    for sql in [
        "INSERT INTO t ( a ) VALUES ( $1 ) RETURNING *",
        "INSERT INTO t ( a ) VALUES ( $1 ) ON CONFLICT ( a ) DO UPDATE SET a = 1 RETURNING id",
        "INSERT INTO t DEFAULT VALUES RETURNING id",
        "INSERT INTO t ( a ) VALUES ( $1 ) ON CONFLICT ( a ) WHERE a > 0 DO UPDATE SET a = 1 RETURNING id",
        "INSERT OR REPLACE INTO t ( a ) VALUES ( max ( ? , 0 ) ) RETURNING id",
        "REPLACE INTO t ( a ) VALUES ( ? ) RETURNING id",
        "WITH v AS ( SELECT 1 ) INSERT INTO t ( a ) VALUES ( $1 ) RETURNING id",
        // A returned column may be named as a keyword read before it.
        "INSERT INTO t ( a ) VALUES ( $1 ) RETURNING a AS do",
    ] {
        assert!(single(sql), "{sql}");
    }
    for sql in [
        "INSERT INTO t ( a ) VALUES ( $1 )",
        "INSERT INTO t ( a ) VALUES ( 1 ) , ( 2 ) RETURNING id",
        "INSERT INTO t ( a ) SELECT a FROM u RETURNING id",
        "INSERT INTO t ( a , values ) SELECT a , b FROM u RETURNING a , values",
        // A column of that name is no `VALUES`.
        "INSERT INTO t ( a ) SELECT u . values FROM u WHERE u . id IN ( 1 , 2 ) RETURNING id",
        "INSERT INTO t ( a ) VALUES ( $1 ) ON CONFLICT DO NOTHING RETURNING id",
        "INSERT INTO t ( a ) VALUES ( $1 ) ON CONFLICT ( a ) DO UPDATE SET a = 1 WHERE t . b RETURNING id",
        "INSERT OR IGNORE INTO t ( a ) VALUES ( ? ) RETURNING id",
    ] {
        assert!(!single(sql), "{sql}");
    }
}

// -- The tables every row has a row of ----------------------------------------

fn proven(sql: &str) -> Vec<String> {
    present(&tokens(sql), &AGGREGATES)
}

#[test]
fn a_table_named_outright_or_inner_joined_is_present() {
    assert_eq!(
        proven("SELECT * FROM a x JOIN s . b ON p , c CROSS JOIN \"D\" AS d NATURAL JOIN e"),
        ["a", "b", "c", "D", "e"]
    );
    // The list ends at its own `FROM`.
    assert_eq!(proven("SELECT a IS DISTINCT FROM b FROM t WHERE c"), ["t"]);
    assert!(proven("SELECT 1").is_empty());
}

#[test]
fn a_table_an_outer_join_may_leave_without_a_row_is_not() {
    assert_eq!(
        proven("SELECT * FROM a JOIN b ON x LEFT OUTER JOIN public . c ON y LEFT JOIN d ON z"),
        ["a", "b"]
    );
    // An inner join after it is of what the outer join left.
    assert_eq!(
        proven("SELECT * FROM a LEFT JOIN b ON left ( a . x , 2 ) = b . y JOIN c ON z"),
        ["a", "c"]
    );
    // The clause ends at no column's name: the outer join after it is read.
    assert_eq!(
        proven("SELECT * FROM a JOIN k ON k . order = a . id LEFT JOIN b ON z"),
        ["a", "k"]
    );
    // Named twice, either may be the one that is missing.
    assert!(proven("SELECT * FROM a x LEFT JOIN a y ON z").is_empty());
    for sql in [
        "SELECT * FROM a RIGHT JOIN b ON x",
        "SELECT * FROM a FULL OUTER JOIN b ON x",
    ] {
        assert!(proven(sql).is_empty(), "{sql}");
    }
}

#[test]
fn a_name_that_may_be_anothers_proves_no_table_present() {
    for sql in [
        // An alias that spells a table's name: of a table, a subquery, a
        // function.
        "SELECT * FROM b AS x LEFT JOIN c AS b ON y",
        "SELECT * FROM b x LEFT JOIN ( SELECT * FROM b ) AS b ON y",
        "SELECT * FROM b AS x JOIN ( SELECT * FROM b ) AS b ON true",
        "SELECT * FROM b AS x JOIN recent_b ( ) AS b ON true",
        // A CTE of a table's name.
        "WITH b AS ( SELECT 1 ) SELECT * FROM b",
    ] {
        assert!(proven(sql).is_empty(), "{sql}");
    }
    // A table under its own name, or beside a CTE, is itself.
    assert_eq!(proven("SELECT * FROM a AS a JOIN b b ON x"), ["a", "b"]);
    assert_eq!(
        proven("WITH v AS ( SELECT 1 ) SELECT * FROM a JOIN v ON x"),
        ["a"]
    );
    // A subquery is no table, nor is a function called for its rows.
    assert_eq!(
        proven("SELECT * FROM a LEFT JOIN LATERAL ( SELECT * FROM b LIMIT 1 ) b ON true"),
        ["a"]
    );
    assert_eq!(
        proven("SELECT * FROM a JOIN ( SELECT * FROM b ) s ON x , generate_series ( 1 , 3 ) g"),
        ["a"]
    );
}

#[test]
fn a_from_that_is_not_read_proves_no_table_present() {
    for sql in [
        // A join in parentheses.
        "SELECT * FROM a LEFT JOIN ( b JOIN c ON x ) ON y",
        "SELECT * FROM ( a LEFT JOIN b ON x ) JOIN c ON y",
        // A join inside another's, without them.
        "SELECT * FROM a LEFT JOIN b JOIN c ON c . b = b . id ON b . a = a . id",
        "SELECT * FROM a LEFT JOIN b CROSS JOIN c ON b . a = a . id",
        // A set operation, whose branches differ.
        "( SELECT * FROM a LEFT JOIN b ON x ) UNION ALL ( SELECT * FROM a JOIN b ON y )",
    ] {
        assert!(proven(sql).is_empty(), "{sql}");
    }
}

#[test]
fn a_statement_that_returns_the_rows_it_writes_has_its_table() {
    for sql in [
        "INSERT INTO t ( a ) VALUES ( 1 ) RETURNING *",
        "INSERT INTO t AS x ( a ) SELECT a FROM u LEFT JOIN t AS old ON y RETURNING *",
        "UPDATE ONLY s . t SET a = 1 RETURNING a",
        "DELETE FROM t WHERE a RETURNING a",
        "WITH v AS ( SELECT 1 ) UPDATE t SET a = 1 FROM v RETURNING a",
    ] {
        assert_eq!(proven(sql), ["t"], "{sql}");
    }
    assert!(proven("UPDATE t SET a = 1").is_empty());
    // An `UPDATE` reads its `FROM` beside the table it writes, which that
    // may name again.
    assert_eq!(
        proven("UPDATE t SET a = u . a FROM u LEFT JOIN v ON x WHERE t . id = u . id RETURNING *"),
        ["t", "u"]
    );
    assert_eq!(
        proven(
            "UPDATE t AS target SET a = 1 FROM u LEFT JOIN t ON t . id = u . id \
             WHERE target . id = u . t RETURNING *"
        ),
        ["u"]
    );
}

#[test]
fn a_row_of_no_rows_has_no_table_where_a_column_may_stand_beside_an_aggregate() {
    let sql = "SELECT a . id , max ( b . id ) FROM a JOIN b ON x";
    assert!(present(&tokens(sql), &SQLITE).is_empty());
    assert_eq!(proven(sql), ["a", "b"]);
    let grouped = "SELECT a . id , max ( b . id ) FROM a JOIN b ON x GROUP BY a . id";
    assert_eq!(present(&tokens(grouped), &SQLITE), ["a", "b"]);
}
