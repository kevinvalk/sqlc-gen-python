//! The sqlc plugin entrypoint.
//!
//! sqlc invokes a WASM plugin as a one-shot process: the `GenerateRequest`
//! arrives on stdin as protobuf and the `GenerateResponse` goes back on stdout.
//! Everything else lives in the library so it can be tested without a WASM
//! runtime.

use std::io::{self, Read, Write};

use anyhow::{Context, Result};
use prost::Message;
use sqlc_gen_python::{generate, plugin};

fn main() -> Result<()> {
    let mut request = Vec::new();
    io::stdin()
        .lock()
        .read_to_end(&mut request)
        .context("reading the request from stdin")?;
    let request =
        plugin::GenerateRequest::decode(&*request).context("decoding the GenerateRequest")?;

    let response = plugin::GenerateResponse {
        files: generate(&request)?,
    };

    io::stdout()
        .write_all(&response.encode_to_vec())
        .context("writing the response to stdout")
}
