use anyhow::{Error, Result, anyhow, bail};

use crate::engine::Engine;

pub(crate) fn check_macros(engine: Engine, words: &[String]) -> Result<()> {
    let is_call = |call: &&[String]| {
        call[0].eq_ignore_ascii_case("sqlc")
            && call[1] == "."
            && (["arg", "narg", "slice", "embed"].iter()).any(|name| call[2].eq_ignore_ascii_case(name))
            && call[3] == "("
    };
    if let Some(call) = words.windows(4).find(is_call) {
        bail!("sqlc did not rewrite {}{}", call[..3].concat(), unrewritten(engine));
    }
    Ok(())
}

/// A hint for the error when a statement's placeholders are not the parameters sqlc reported.
pub(crate) fn misnumbered(engine: Engine) -> &'static str {
    match engine {
        Engine::PostgreSql => "",
        // sqlc numbers a `sqlc.arg` among `?`s, and a `sqlc.slice` used twice, differently than SQLite does
        Engine::Sqlite => {
            "; on SQLite, name every parameter of the statement with sqlc.arg rather than mix them with ?, \
             and use a sqlc.slice once; in some clauses, such as HAVING, ORDER BY and \
             ON CONFLICT ... DO UPDATE, sqlc reads no parameter at all"
        }
        // sqlc reports no parameter for the first two, and an executable comment is no code to our lexer
        Engine::MySql => {
            "; on MySQL, sqlc reads no parameter in some subqueries, such as > ANY (SELECT ...), nor \
             a sqlc.slice without a plain column on its left, and we read no placeholder inside /*! ... */"
        }
    }
}

/// A hint for the error about a named parameter sqlc left as written: where sqlc 1.31.1 reads none.
fn unrewritten(engine: Engine) -> &'static str {
    match engine {
        Engine::PostgreSql => {
            ", as it rewrites none in a statement it does not analyse, such as MERGE; \
             write it as INSERT ... ON CONFLICT, or UPDATE ... FROM"
        }
        Engine::Sqlite => {
            ", as on SQLite it reads no parameter in some clauses, such as HAVING, ORDER BY and \
             ON CONFLICT ... DO UPDATE; write the row's own value as excluded.<column> in a DO UPDATE, \
             and filter groups in a WHERE around a subquery rather than in a HAVING"
        }
        Engine::MySql => {
            ", as on MySQL it reads no parameter in some subqueries, such as > ANY (SELECT ...) and \
             > ALL (SELECT ...); write = ANY as IN (SELECT ...), or compare with (SELECT min(...)) or \
             (SELECT max(...))"
        }
    }
}

pub(crate) fn named_placeholder(engine: Engine, name: &str) -> Error {
    anyhow!("sqlc did not rewrite {name} into a placeholder{}", unrewritten(engine))
}
