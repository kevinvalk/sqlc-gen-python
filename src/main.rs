mod codegen;
mod sqlc;

use anyhow::Result;
use prost::Message;
use sqlc::{Config, plugin};

use std::io::{self, Cursor, Read, Write};

pub fn main() -> Result<()> {
    // We receive the "RPC" via stdin, so to get the request we have to decode the received protobuf from stdin.
    let mut buffer = Vec::new();
    io::stdin().lock().read_to_end(&mut buffer)?;
    let request = plugin::GenerateRequest::decode(Cursor::new(buffer))?;

    // We want to support custom options and configuration for our plugin, so let's use the generic config field for this.
    let config = Config::from_option(&request.plugin_options)?;

    // Generate all required files and as we are still learning add the SQLC request during debugging.
    let mut files = codegen::generate_files(&config, &request)?;
    if config.debug {
        let req_txt = format!("{request:#?}");
        files.push(plugin::File {
            name: "input.txt".into(),
            contents: req_txt.into_bytes(),
        });
    }

    // To communicate our results back to SQLC we serialize our response and write it out via stdout. Remember, we are just
    // a WASM module running within SQLC.
    let response = plugin::GenerateResponse { files: files };
    std::io::stdout().write_all(&response.encode_to_vec())?;

    Ok(())
}
