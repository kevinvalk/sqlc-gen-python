//! `cargo xtask generate REQUEST OPTIONS [OUT]`: the plugin run on a request
//! as JSON (`tests/sqlc/...`), with options as JSON, printing each file, or
//! writing them into OUT. For trying an input without sqlc.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use sqlc_gen_python::plugin;

pub fn run(request: &str, options: &str, out: Option<&str>) -> Result<()> {
    let text = fs::read_to_string(request).with_context(|| format!("reading {request}"))?;
    let mut request: plugin::GenerateRequest =
        serde_json::from_str(&text).with_context(|| format!("parsing {request}"))?;
    request.plugin_options = options.as_bytes().to_vec();
    let settings = request.settings.get_or_insert_with(Default::default);
    settings.codegen.get_or_insert_with(|| plugin::Codegen {
        out: "db".into(),
        plugin: "python".into(),
        ..Default::default()
    });
    let files = sqlc_gen_python::generate(&request)?;
    for file in files {
        let contents = String::from_utf8_lossy(&file.contents);
        match out {
            Some(out) => {
                let path = Path::new(out).join(&file.name);
                fs::create_dir_all(path.parent().unwrap_or(Path::new(out)))?;
                fs::write(&path, contents.as_bytes())?;
            }
            None => println!("##### {}\n{contents}", file.name),
        }
    }
    Ok(())
}
