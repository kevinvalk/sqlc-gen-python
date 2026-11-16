use super::*;
use crate::engine::Engine;

/// PostgreSQL's: an aggregate whatever its arguments, and no column beside an ungrouped one.
const AGGREGATES: Aggregates = Aggregates {
    nullable: &["sum", "avg", "min", "max", "string_agg"],
    never_null: &["count", "total"],
    scalar_of_several: &[],
    bare_columns: false,
};

const SQLITE: Aggregates = Aggregates {
    scalar_of_several: &["min", "max"],
    bare_columns: true,
    ..AGGREGATES
};

/// We take tokens from a real lexer, as a reader proven on tokens made by hand proves nothing of real statements.
fn tokens(sql: &str) -> Vec<String> {
    Engine::PostgreSql.words(sql)
}

/// The items of `sql`'s list, sqlc reporting each of the `columns` as of no table.
fn read(sql: &str, columns: usize) -> Option<Vec<Item>> {
    items(&tokens(sql), &vec![""; columns], &AGGREGATES)
}

fn counts(sql: &str, columns: usize) -> Vec<bool> {
    let items = read(sql, columns).expect(sql);
    items.into_iter().map(|item| item.count).collect()
}

fn nullable(sql: &str, columns: usize) -> Vec<bool> {
    let items = read(sql, columns).expect(sql);
    items.into_iter().map(|item| item.nullable).collect()
}

#[test]
fn an_item_is_a_count_where_it_is_a_bare_call_of_it() {
    assert_eq!(
        counts("SELECT id , COUNT ( * ) , count ( x ) AS n , sum ( y ) total FROM t", 4),
        [false, true, true, false]
    );
    assert_eq!(
        counts("SELECT DISTINCT ON ( a ) a , count ( * ) FROM t", 2),
        [false, true]
    );
    // The main statement's list, after a CTE's own.
    assert_eq!(
        counts("WITH c AS ( SELECT sum ( x ) FROM t ) SELECT count ( * ) FROM c", 1),
        [true]
    );
    assert_eq!(
        counts("INSERT INTO t ( a ) VALUES ( 1 ) RETURNING id , count ( * )", 2),
        [false, true]
    );
    // And behind a scalar subquery's parentheses, alone.
    assert_eq!(counts("SELECT ( SELECT count ( * ) FROM t ) n", 1), [true]);
}

#[test]
fn a_count_is_one_of_the_rows_a_filter_keeps_or_a_window_holds() {
    for sql in [
        "SELECT count(*) FILTER (WHERE x) FROM t",
        "SELECT count(*) OVER () AS total FROM t",
        "SELECT count(*) OVER w FROM t WINDOW w AS (PARTITION BY a)",
        "SELECT count(x) FILTER (WHERE y > 0) OVER (PARTITION BY a ORDER BY b) n FROM t",
        "SELECT (SELECT count(*) FILTER (WHERE x) FROM t)",
        "SELECT (SELECT count(*) OVER () FROM t LIMIT 1)",
    ] {
        assert_eq!(counts(sql, 1), [true], "{sql}");
    }
    // Neither is NULL, but a window's rows are not one row, so a subquery of a count over one may have no row
    assert_eq!(
        nullable(
            "SELECT count(*) FILTER (WHERE x), count(*) OVER (), \
             (SELECT count(*) FILTER (WHERE x) FROM u), (SELECT count(*) OVER () FROM u) \
             FROM t",
            4
        ),
        [false, false, false, true]
    );
    // SQLite reads either word with nothing of its own after it as an alias, so the call stays an aggregate
    let sqlite = |sql: &str| items(&Engine::Sqlite.words(sql), &[""], &SQLITE).expect(sql);
    for alias in ["over", "filter"] {
        let sql = format!("SELECT max(x) {alias} FROM t");
        assert!(sqlite(&sql)[0].nullable, "{sql}");
        assert!(!single(&sql.replace("max(x)", "x")), "{sql}");
        assert!(single(&sql), "{sql}");
    }
}

#[test]
fn a_call_with_more_after_it_is_no_count() {
    for sql in [
        "SELECT count ( * ) + 1 FROM t",
        "SELECT count ( * ) :: int FROM t",
        "SELECT count ( * ) OVER ( ) + 1 FROM t",
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
    // One such branch of a set operation is enough, as is one without a list
    assert_eq!(read("SELECT a FROM t UNION SELECT a , b FROM u", 1), None);
    assert_eq!(
        read("SELECT ( SELECT a FROM t LIMIT 1 ) UNION ALL VALUES ( 'x' )", 1),
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
        ("SELECT ( SELECT a FROM t LIMIT 1 ) AS within FROM k", &[true]),
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
        counts("SELECT count ( * ) FROM a UNION ALL SELECT count ( * ) FROM b", 1),
        [true]
    );
    assert_eq!(counts("SELECT count ( * ) FROM a UNION SELECT 1 FROM b", 1), [false]);
    // NULL in any branch is NULL in the result, a branch in parentheses too.
    for sql in [
        "SELECT a FROM t UNION ALL SELECT ( SELECT b FROM u LIMIT 1 ) FROM t",
        "( SELECT ( SELECT b FROM u LIMIT 1 ) FROM t ) UNION ALL ( SELECT a FROM t ) ORDER BY 1",
    ] {
        assert_eq!(nullable(sql, 1), [true], "{sql}");
    }
    // What an `INSERT` selects is no branch of it, as its list is what it returns
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
fn a_set_operations_word_that_is_a_name_splits_nothing() {
    assert_eq!(
        nullable("SELECT t.union, (SELECT max(x) FROM u) FROM t", 2),
        [false, true]
    );
    assert!(single("SELECT count(*) AS union FROM t"));
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
        "SELECT (((SELECT count(*) FROM t)) LIMIT 0) n",
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
    // A group has a row, and a window function is of each row, so sqlc's report stands
    for sql in [
        "SELECT sum ( x ) :: int FROM t GROUP BY a",
        "SELECT sum ( x ) OVER ( ) FROM t",
        "SELECT sum ( x ) FILTER ( WHERE a ) OVER ( ) FROM t",
    ] {
        assert_eq!(nullable(sql, 1), [false], "{sql}");
    }
    // These still make a row of no rows: the empty grouping set, a `HAVING` or a `WINDOW` alone, another call's
    // `OVER`, or a column named `over`
    for sql in [
        "SELECT sum ( x ) :: int FROM t GROUP BY ( )",
        "SELECT sum(x)::int FROM t GROUP BY DISTINCT ()",
        "SELECT sum(x)::int FROM t GROUP BY ALL ()",
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
fn a_case_is_null_where_it_may_take_no_branch_or_a_null_one() {
    // sqlc types a `CASE` by its `ELSE`, and reports every cast NOT NULL
    for sql in [
        "SELECT CASE WHEN a THEN b END FROM t",
        "SELECT (CASE WHEN a THEN b END)::int AS n FROM t",
        "SELECT CASE WHEN a THEN b ELSE NULL END FROM t",
        "SELECT CASE WHEN a THEN NULL::text WHEN c THEN d ELSE '' END FROM t",
        "SELECT CASE k WHEN 1 THEN CAST(NULL AS text) ELSE '' END FROM t",
        // Wherever it stands in the item, and in another's branch.
        "SELECT 1 + CASE WHEN a THEN b END FROM t",
        "SELECT CASE WHEN a THEN CASE WHEN c THEN d END ELSE 0 END FROM t",
        "SELECT CASE WHEN a THEN b",
    ] {
        assert_eq!(nullable(sql, 1), [true], "{sql}");
    }
    for sql in [
        "SELECT CASE WHEN a THEN b ELSE c END FROM t",
        "SELECT CASE WHEN a IS NULL THEN 0 WHEN b IS NOT NULL THEN 1 ELSE 2 END FROM t",
        "SELECT CASE WHEN a THEN CASE WHEN c THEN d ELSE e END ELSE 0 END FROM t",
        // A name, whatever it spells; and a default guards it.
        "SELECT CASE WHEN t.end THEN t.case ELSE 0 END AS end FROM t",
        "SELECT coalesce(CASE WHEN a THEN b END, 0) FROM t",
        // What a count counts does not make the count NULL
        "SELECT count(CASE WHEN a THEN 1 END) FROM t",
        "SELECT count(DISTINCT CASE WHEN a THEN b ELSE NULL END) AS n FROM t GROUP BY k",
        "SELECT sum(CASE WHEN a THEN 1 ELSE 0 END)::int FROM t GROUP BY k",
    ] {
        assert_eq!(nullable(sql, 1), [false], "{sql}");
    }
    // A sum of a group none of whose rows takes a branch is NULL.
    assert_eq!(
        nullable("SELECT sum(CASE WHEN a THEN 1 END)::int FROM t GROUP BY k", 1),
        [true]
    );
    assert_eq!(counts("SELECT count(CASE WHEN a THEN 1 END) FROM t", 1), [true]);
}

#[test]
fn the_literal_null_is_null_whatever_its_cast() {
    assert_eq!(
        nullable(
            "SELECT NULL, NULL AS a, NULL::text AS b, NULL::numeric(12, 2), CAST(NULL AS text) c",
            5
        ),
        [true; 5]
    );
    // In any branch of a set operation.
    assert_eq!(nullable("SELECT name FROM a UNION ALL SELECT NULL", 1), [true]);
    assert_eq!(
        nullable("SELECT NULL IS NULL, 'NULL', nullable, t.null FROM t", 4),
        [false; 4]
    );
}

#[test]
fn a_coalesce_guards_only_with_a_default() {
    for sql in [
        "SELECT coalesce ( sum ( x ) , 0 ) :: int FROM t",
        "SELECT coalesce ( sum ( x ) , sum ( y ) , 0 :: numeric ( 12 , 2 ) ) FROM t",
        "SELECT coalesce(sum(x), 0::numeric), coalesce(max(name), ''::text) FROM t",
        "SELECT coalesce ( max ( x ) , - 1 ) FROM t",
        "SELECT coalesce(avg(x), 0.0), coalesce(max(x), -1.5::float8) FROM t",
        "SELECT ifnull ( max ( x ) , '' ) FROM t",
        "SELECT coalesce ( ( SELECT a FROM u LIMIT 1 ) , false ) FROM t",
    ] {
        let columns = sql.matches("coalesce").count().max(1);
        assert_eq!(nullable(sql, columns), vec![false; columns], "{sql}");
    }
    for sql in [
        "SELECT coalesce ( max ( x ) , max ( y ) ) :: int FROM t",
        "SELECT coalesce ( max ( x ) ) FROM t",
        "SELECT coalesce ( max ( x ) , 0 + max ( y ) ) FROM t",
        "SELECT coalesce ( max ( x ) , 0 :: int OR max ( y ) ) FROM t",
        "SELECT coalesce ( ( SELECT a FROM u LIMIT 1 ) , t . b ) FROM t",
        "SELECT coalesce(max(x), -'a') FROM t",
    ] {
        assert_eq!(nullable(sql, 1), [true], "{sql}");
    }
    // MySQL's `2nd` is a column, not a number.
    let mysql = "SELECT coalesce(max(a), 2nd) FROM t";
    let items = items(&Engine::MySql.words(mysql), &[""], &AGGREGATES).expect(mysql);
    assert!(items[0].nullable);
}

#[test]
fn a_column_beside_an_ungrouped_aggregate_is_null_of_no_rows_where_one_may_stand() {
    let read = |sql: &str, aggregates: &Aggregates| -> Vec<bool> {
        let items = items(&tokens(sql), &[""; 3], aggregates).expect(sql);
        items.into_iter().map(|item| item.nullable).collect()
    };
    let bare = "SELECT id , CAST ( max ( x ) AS INTEGER ) AS m , count ( * ) FROM k";
    assert_eq!(read(bare, &SQLITE), [true, true, false]);
    assert_eq!(read(bare, &AGGREGATES), [false, true, false]);
    let grouped = "SELECT id , CAST ( max ( x ) AS INTEGER ) AS m , count ( * ) FROM k GROUP BY id";
    assert_eq!(read(grouped, &SQLITE), [false, false, false]);
    // SQLite's `max` of several arguments is a function of each row, not an aggregate
    let scalar = "SELECT id , max ( a , b ) , min ( a , b ) FROM k";
    assert_eq!(read(scalar, &SQLITE), [false, false, false]);
    // A `HAVING` alone aggregates the rows too, as MySQL lets it.
    let having = "SELECT id, name, x FROM k HAVING count(*) >= 0";
    assert_eq!(read(having, &SQLITE), [true, true, true]);
    assert!(present(&tokens(having), &SQLITE).is_empty());
}

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
        // Between parentheses, what limits it is as much its own.
        "((SELECT count(*) FROM t) LIMIT 0)",
        "(((SELECT count(*) FROM t)) OFFSET 1)",
        // A number's dot names nothing after it.
        "SELECT count(*) FROM t WHERE f > 1. GROUP BY y",
        "SELECT count(*) FROM t WHERE f > 1. LIMIT 0",
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
    // A schema's own function named like an aggregate is not one
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
fn a_join_after_what_ended_the_from_proves_it_was_no_end() {
    // MySQL and SQLite allow a column named as a clause, and we would miss the outer join after it
    let sql = "SELECT a.id FROM a JOIN b ON offset = 1 RIGHT JOIN c ON c.id = a.id";
    assert!(present(&Engine::MySql.words(sql), &AGGREGATES).is_empty());
    assert_eq!(
        proven("SELECT a.id FROM a JOIN b ON b.x = 1 WHERE EXISTS (SELECT 1 FROM c JOIN d ON y)"),
        ["a", "b"]
    );
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
    // A right join may leave what stands before it (back to the last comma) without a row, and a full one what it
    // brings too
    assert_eq!(proven("SELECT * FROM a RIGHT JOIN b ON x"), ["b"]);
    assert_eq!(
        proven("SELECT * FROM k, a JOIN b ON x RIGHT OUTER JOIN c ON y JOIN d ON z"),
        ["k", "c", "d"]
    );
    assert_eq!(
        proven("SELECT * FROM a JOIN b ON x FULL OUTER JOIN c ON y JOIN d ON z"),
        ["d"]
    );
}

/// Whether an outer join may leave each item of `sql`'s list NULL, sqlc reporting each as a column of its table in
/// `tables`.
fn outer_of(sql: &str, tables: &[&str]) -> Vec<bool> {
    let items = items(&tokens(sql), tables, &AGGREGATES).expect(sql);
    items.into_iter().map(|item| item.nullable).collect()
}

#[test]
fn a_column_is_null_where_every_source_it_may_be_of_may_be_without_a_row() {
    // sqlc reports the columns of `a` and `b` NOT NULL, on every engine.
    assert_eq!(
        outer_of(
            "SELECT a.x, b.y, c.z, 1 FROM a JOIN b ON x RIGHT JOIN c ON y",
            &["a", "b", "c", ""]
        ),
        [true, true, false, false]
    );
    // And on SQLite and MySQL, those of `b` here.
    for join in ["JOIN", "NATURAL JOIN", "STRAIGHT_JOIN"] {
        let sql = format!("SELECT x, v, z FROM a LEFT JOIN public.\"B\" ON y {join} c");
        assert_eq!(outer_of(&sql, &["a", "b", "c"]), [false, true, false], "{sql}");
    }
    assert_eq!(
        outer_of("SELECT x, y, z FROM a FULL JOIN b ON x, c", &["a", "b", "c"]),
        [true, true, false]
    );
    // An `UPDATE`'s own table has its row.
    assert_eq!(
        outer_of(
            "UPDATE a SET x = 1 FROM b LEFT JOIN c ON y WHERE b.id = a.id RETURNING a.x, b.y, c.v",
            &["a", "b", "c"]
        ),
        [false, false, true]
    );
}

#[test]
fn a_bare_column_is_of_the_table_in_the_schema_sqlc_reports() {
    // Two tables of one name are two sources only in two schemas, which sqlc tells apart and so must we
    let sql = "SELECT label FROM a.items AS x LEFT JOIN b.items AS y ON y.id = x.id";
    assert_eq!(outer_of(sql, &["b.items"]), [true]);
    assert_eq!(outer_of(sql, &["a.items"]), [false]);
    // A source the statement does not qualify is in whichever schema the search path finds
    assert_eq!(
        outer_of("SELECT label FROM a LEFT JOIN items ON z", &["data.items"]),
        [true]
    );
}

#[test]
fn a_qualified_column_is_its_qualifiers_source_and_a_bare_one_its_tables() {
    // sqlc names the table whatever its alias, so only a qualifier tells which of two sources of one table it is
    let boss = "SELECT e.name, m.name AS boss, name FROM staff e LEFT JOIN staff m ON m.id = e.boss";
    assert_eq!(outer_of(boss, &["staff", "staff", "staff"]), [false, true, false]);
    assert_eq!(
        outer_of("SELECT public.b.v, b.v FROM a x LEFT JOIN b ON z", &["b", "b"]),
        [true, true]
    );
    // Under an alias, a table's own name is not its source's.
    assert_eq!(
        outer_of("SELECT y.v, b.v FROM a x LEFT JOIN b AS y ON z", &["b", "b"]),
        [true, false]
    );
    // Without a qualifier, both sources of its table must be without a row.
    assert_eq!(
        outer_of(
            "SELECT name FROM k LEFT JOIN staff e ON x LEFT JOIN staff m ON y",
            &["staff"]
        ),
        [true]
    );
}

#[test]
fn a_subquerys_column_is_its_aliass_or_without_a_qualifier_of_any_subquery() {
    // sqlc reports the column of `s` as `b`'s, NOT NULL.
    assert_eq!(
        outer_of(
            "SELECT a.x, s.v, v FROM a LEFT JOIN (SELECT v FROM b) s ON y",
            &["a", "b", "b"]
        ),
        [false, true, true]
    );
    assert_eq!(
        outer_of(
            "SELECT s.v FROM a LEFT JOIN LATERAL ((SELECT v FROM b) LIMIT 1) s ON true",
            &["b"]
        ),
        [true]
    );
    // One of several subqueries may be the one that has its row.
    assert_eq!(
        outer_of(
            "SELECT v, w, r.w FROM (SELECT v FROM b) s LEFT JOIN (SELECT w FROM c) r ON y",
            &["b", "c", "c"]
        ),
        [false, false, true]
    );
}

#[test]
fn only_a_bare_column_is_null_by_an_outer_join() {
    // To sqlc a `coalesce` of a column is the column, and rightly NOT NULL.
    assert_eq!(
        outer_of(
            "SELECT coalesce(b.v, 0) AS v, b.v + 1, b.v::text, b.v FROM a LEFT JOIN b ON y",
            &["b", "b", "b", "b"]
        ),
        [false, false, false, true]
    );
}

#[test]
fn a_from_that_is_not_read_leaves_each_column_as_sqlc_reports_it() {
    for sql in [
        "SELECT a.x, b.y, c.z FROM a LEFT JOIN (b JOIN c ON x) ON y",
        "SELECT a.x, b.y, c.z FROM a LEFT JOIN b JOIN c ON c.b = b.id ON b.a = a.id",
        "SELECT 1, 2, 3",
    ] {
        assert_eq!(outer_of(sql, &["a", "b", "c"]), [false; 3], "{sql}");
    }
    // Each branch of a set operation is read by its own.
    assert_eq!(
        outer_of(
            "SELECT b.y FROM a JOIN b ON x UNION SELECT b.y FROM a LEFT JOIN b ON x",
            &["b"]
        ),
        [true]
    );
}

#[test]
fn a_name_that_may_be_anothers_proves_no_table_present() {
    for sql in [
        // An alias that spells a table's name: of a table, a subquery, a function
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
    assert_eq!(proven("WITH v AS ( SELECT 1 ) SELECT * FROM a JOIN v ON x"), ["a"]);
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
    // An `UPDATE` reads its `FROM` beside the table it writes, which that `FROM` may name again
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
