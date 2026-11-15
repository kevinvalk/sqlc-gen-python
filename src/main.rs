//! sqlc runs a WASM plugin as a one-shot process with the `GenerateRequest` on stdin and the `GenerateResponse` on
//! stdout. Everything else lives in the library, so we can test it without a WASM runtime.

use std::io::{self, Read, Write};
use std::process::ExitCode;

use anyhow::{Context, Result};
use prost::Message;
use sqlc_gen_python::{generate, plugin};

#[cfg(test)]
#[path = "main_spec.rs"]
mod spec;

fn main() -> ExitCode {
    let Err(error) = run() else {
        return ExitCode::SUCCESS;
    };
    eprintln!("Error: {}", printable(&format!("{error:?}")));
    ExitCode::FAILURE
}

fn run() -> Result<()> {
    let mut request = Vec::new();
    io::stdin()
        .lock()
        .read_to_end(&mut request)
        .context("reading the request from stdin")?;
    let request = plugin::GenerateRequest::decode(&*request).context("decoding the GenerateRequest")?;

    let response = plugin::GenerateResponse {
        files: generate(&request)?,
    };

    io::stdout()
        .write_all(&response.encode_to_vec())
        .context("writing the response to stdout")
}

/// An error quotes the schema's names and the SQL's strings, and sqlc prints it to a terminal, which would obey an
/// escape sequence one of them spells. So we escape every control character but a line break.
fn printable(text: &str) -> String {
    let mut printable = String::with_capacity(text.len());
    for c in text.chars() {
        match c.is_control() && c != '\n' {
            true => printable.extend(c.escape_default()),
            false => printable.push(c),
        }
    }
    printable
}
