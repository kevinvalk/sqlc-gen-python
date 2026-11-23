#[cfg(test)]
#[path = "types_spec.rs"]
mod spec;

use crate::engine::Engine;
use crate::plugin;
use crate::problem::Problem;
use crate::sqlc_typing::{Column, source};

/// Whether `column` is a parameter without a column behind it (of a `LIMIT`, an `OFFSET` or a function), which sqlc
/// types as `integer` itself on every engine.
fn is_typed_by_sqlc(column: &plugin::Column, parameter: bool) -> bool {
    parameter && column.table.is_none() && column.data_type() == "integer"
}

/// Whether sqlc guessed the type of `column` (a query's parameter when `parameter`, else a result's or a table's
/// column) instead of reading it. On MySQL and SQLite a guess is `any` or nothing, for an expression sqlc could
/// not type.
///
/// On PostgreSQL every standard name is a guess ([`postgres_guess`]), and so is a bare `integer`: sqlc spells
/// a column's own type `pg_catalog.int4`, so `integer` is a function's first overload (`age(ts)`, an interval,
/// read through a CTE is the `integer` of `age(xid)`). Except for a parameter [`is_typed_by_sqlc`].
pub(super) fn is_guess(engine: Engine, column: &plugin::Column, parameter: bool) -> bool {
    let data_type = column.data_type();
    match engine {
        Engine::PostgreSql => {
            postgres_guess(&data_type) || (data_type == "integer" && !is_typed_by_sqlc(column, parameter))
        }
        Engine::Sqlite | Engine::MySql => ["", "any"].contains(&data_type.as_str()),
    }
}

pub(crate) fn expression_problem(
    engine: Engine,
    column: &plugin::Column,
    catalog: &plugin::Catalog,
    facts: &Column,
    field_name: &str,
) -> Option<Problem> {
    // Through a subquery, sqlc can report a coalesce as its nullable source column made NOT NULL.
    if let Some((reason, remedy)) = facts.problem {
        return Some(Problem::new(format!("column \"{field_name}\" {reason}"), remedy));
    }
    let name = if column.name.is_empty() {
        field_name
    } else {
        &column.name
    };
    if facts.operand_typed {
        let cast = cast_hint(engine, None);
        Some(Problem::new(
            format!(
                "column \"{name}\" is a coalesce, a nullif or a CASE, which sqlc types by one operand whatever the \
                 others are"
            ),
            format!("add a cast, as {cast}{}", analysis(engine)),
        ))
    } else if column.not_null && source(catalog, column).is_some_and(|source| !source.not_null) {
        Some(Problem::new(
            format!("column \"{name}\" has a type sqlc could not infer"),
            format!("add a cast, as {}", engine.cast("coalesce(<expr>, <default>)")),
        ))
    } else if is_untyped_expression(engine, column) {
        Some(Problem::new(
            format!("column \"{name}\" is an expression sqlc typed by its form, not its operands"),
            format!("add a cast, as {}", engine.cast("<expr>")),
        ))
    } else {
        None
    }
}

/// A type override can name a declared type. sqlc also writes type names for guesses and for a synthetic LIMIT parameter.
pub(crate) fn allows_type_override(
    engine: Engine,
    column: &plugin::Column,
    parameter: Option<&str>,
    what: &str,
) -> Result<bool, Problem> {
    if column.is_func_call {
        Err(Problem::new(
            format!(
                "{what} is a function's result, which sqlc types by the function's first overload whatever its \
                 arguments (sum() of a numeric column as bigint)"
            ),
            format!("add a cast, as {}", cast_hint(engine, parameter)),
        ))
    } else if is_guess(engine, column, parameter.is_some()) {
        Err(Problem::new(
            format!("{what} has a type sqlc could not infer"),
            format!("add a cast, as {}{}", cast_hint(engine, parameter), analysis(engine)),
        ))
    } else {
        Ok(!is_typed_by_sqlc(column, parameter.is_some()))
    }
}

fn cast_hint(engine: Engine, parameter: Option<&str>) -> String {
    match parameter {
        Some(name) => {
            let bind = match engine {
                Engine::PostgreSql => format!("@{name}"),
                Engine::MySql | Engine::Sqlite => format!("sqlc.arg({name})"),
            };
            format!(
                "{}, or {} if it can be NULL",
                engine.cast(&bind),
                engine.cast(&format!("sqlc.narg('{name}')"))
            )
        }
        None => format!(
            "{}, or {} if it can be NULL, as sqlc reports every cast NOT NULL",
            engine.cast("<expr>"),
            engine.cast("coalesce(<expr>, <default>)")
        ),
    }
}

/// Whether a result `column` is an expression sqlc typed by its form instead of its operands. On MySQL and SQLite
/// any arithmetic is `int` (`price * 1.1` too), MySQL's `1.0` (a `DECIMAL`) is `float` and its `BINARY x` (bytes)
/// is `char`. Neither a cast nor a column lands here, as a cast keeps its declared spelling (`INTEGER`, `SIGNED`,
/// `REAL`, `DOUBLE`) and a column has its table.
fn is_untyped_expression(engine: Engine, column: &plugin::Column) -> bool {
    let forms: &[&str] = match engine {
        Engine::PostgreSql => &[],
        Engine::Sqlite => &["int", "float"],
        Engine::MySql => &["int", "float", "char"],
    };
    column.table.is_none() && forms.contains(&column.data_type().as_str())
}

/// Whether `data_type` is sqlc's guess instead of a type it read.
///
/// sqlc's static analyser writes a declared or cast type by PostgreSQL's own name, `pg_catalog`-qualified where the
/// grammar spells it as a keyword (`bigint` is `pg_catalog.int8`, `timestamptz` stays bare). It only writes SQL's
/// standard name for a type it took off a function's signature: the first overload with that many arguments, whatever
/// the call resolves to. That guess survives a view, a CTE and a subquery, where `is_func_call` does not. A bare `int`
/// (its type for any arithmetic), `any` and nothing are guesses too. `integer` alone is not, as sqlc types some
/// parameters with it itself ([`is_guess`]).
fn postgres_guess(data_type: &str) -> bool {
    [
        "",
        "any",
        "anyelement",
        "anyarray",
        "int",
        "smallint",
        "bigint",
        "real",
        "double precision",
        "numeric",
        "boolean",
        "character",
        "character varying",
        "interval",
        "time with time zone",
        "time without time zone",
        "timestamp with time zone",
        "timestamp without time zone",
    ]
    .contains(&data_type)
}

/// A hint for the error about a guessed type. On PostgreSQL, sqlc's database analyzer names every type the way its
/// static analysis names only its guesses, so we cannot tell them apart.
fn analysis(engine: Engine) -> &'static str {
    match engine {
        Engine::PostgreSql => {
            "; with a database block in sqlc.yaml, also set analyzer: {database: false}, \
             as only sqlc's static analysis can be told from a guess"
        }
        Engine::Sqlite | Engine::MySql => "",
    }
}

/// A hint for the error about a column of `data_type` that has no Python type, or nothing.
pub(crate) fn unmapped_hint(engine: Engine, data_type: &str) -> &'static str {
    match engine {
        // sqlc sends a domain's name, never its base type.
        Engine::PostgreSql if !data_type.starts_with("pg_catalog.") => {
            ", or, for a domain, declare the column with its base type and a CHECK constraint"
        }
        Engine::Sqlite => {
            "; but SQLite stores a value of its NUMERIC affinity as whichever of an integer, \
                 a real and text it looks like, so better declare it INTEGER, REAL, TEXT or BLOB"
        }
        Engine::MySql if data_type == "var_string" => {
            "; it is how sqlc reports a CAST AS CHAR, a str, and a CAST AS BINARY, bytes, alike"
        }
        Engine::PostgreSql | Engine::MySql => "",
    }
}
