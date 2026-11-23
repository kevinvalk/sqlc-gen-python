use std::io::{self, BufRead, Write};

use anyhow::Result;

use sqlc_gen_python::backend::sqlalchemy::text::{check, text};
use sqlc_gen_python::engine::{Bind, Engine};

/// Answers each stdin line, a JSON `[sql, [[number, name], ...]]`, with a line `{"Ok": text}` or `{"Err": message}`:
/// PostgreSQL's cutting of `sql` at its placeholders, then SQLAlchemy's check and [`text`] of it.
/// `tests/python/scanner.py` uses this for its differential check against PostgreSQL's lexer and SQLAlchemy.
pub fn serve() -> Result<()> {
    let mut stdout = io::stdout().lock();
    for line in io::stdin().lock().lines() {
        let (sql, parameters): (String, Vec<(i32, String)>) = serde_json::from_str(&line?)?;
        let binds: Vec<Bind> = (parameters.iter())
            .enumerate()
            .map(|(argument, (number, name))| Bind {
                argument,
                number: *number,
                name,
                slice: None,
            })
            .collect();
        let result = (Engine::PostgreSql.sql(&sql, &binds))
            .and_then(|sql| check(&sql).map(|()| text(&sql)))
            .map_err(|error| format!("{error:#}"));
        serde_json::to_writer(&mut stdout, &result)?;
        writeln!(stdout)?;
        stdout.flush()?;
    }
    Ok(())
}
