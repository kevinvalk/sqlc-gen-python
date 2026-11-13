//! PostgreSQL's cutting of a statement at its placeholders, then SQLAlchemy's
//! check and [`text`] of it, over stdin, for their
//! differential check against PostgreSQL's lexer and SQLAlchemy
//! (`tests/python/scanner.py`): each line a JSON
//! `[sql, [[number, name], ...]]`, answered by a line `{"Ok": text}` or
//! `{"Err": message}`.

use std::io::{self, BufRead, Write};

use anyhow::Result;

use sqlc_gen_python::backend::sqlalchemy::text::{check, text};
use sqlc_gen_python::engine::Engine;

pub fn serve() -> Result<()> {
    let mut out = io::stdout().lock();
    for line in io::stdin().lock().lines() {
        let (sql, params): (String, Vec<(i32, String)>) = serde_json::from_str(&line?)?;
        let params: Vec<(i32, &str)> = (params.iter())
            .map(|(n, name)| (*n, name.as_str()))
            .collect();
        let result = (Engine::PostgreSql.sql(&sql, &params))
            .and_then(|sql| check(&sql).map(|()| text(&sql)))
            .map_err(|error| format!("{error:#}"));
        serde_json::to_writer(&mut out, &result)?;
        writeln!(out)?;
        out.flush()?;
    }
    Ok(())
}
