//! The fixtures the tests read: the golden cases under `examples/`, the
//! requests sqlc sends for them, and `GEN_DIR`, where `cargo xtask` collects
//! what the tests generate.

use std::fs;
use std::path::{Path, PathBuf};

use sqlc_gen_python::plugin;

/// The repository root, as cargo reports it to this run rather than to the
/// build: a `target/` reused by another copy of the tree would otherwise test
/// the first copy's fixtures.
pub fn root() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("run by cargo"))
}

/// Reads a JSON fixture, panicking with the path if it is missing or invalid.
pub fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> T {
    let contents = fs::read_to_string(path).unwrap_or_else(|error| {
        panic!("reading {path:?}: {error} (fixtures come from `cargo xtask capture`)")
    });
    serde_json::from_str(&contents).unwrap_or_else(|error| panic!("parsing {path:?}: {error}"))
}

/// A golden case: one `codegen` entry of an example's `sqlc.yaml`, whose
/// `out` directory beside it holds what it generates.
pub struct Case {
    /// The example's directory, under `examples/`.
    pub example: PathBuf,
    /// The entry's `out`, a directory named for the case.
    pub name: String,
    /// The entry's `options`, as sqlc hands them to the plugin.
    pub options: serde_json::Value,
}

/// An example's `sqlc.yaml`, as far as the cases need it.
#[derive(serde::Deserialize)]
struct SqlcYaml {
    sql: Vec<Package>,
}

#[derive(serde::Deserialize)]
struct Package {
    #[serde(default)]
    codegen: Vec<Codegen>,
}

#[derive(serde::Deserialize)]
struct Codegen {
    out: String,
    #[serde(default)]
    options: serde_json::Value,
}

/// Every golden case: each `codegen` entry of each `examples/*/sqlc.yaml`.
pub fn cases() -> Vec<Case> {
    let examples = root().join("examples");
    let mut dirs: Vec<PathBuf> = (fs::read_dir(&examples).unwrap())
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.join("sqlc.yaml").is_file())
        .collect();
    dirs.sort();
    let cases: Vec<Case> = (dirs.into_iter())
        .flat_map(|example| {
            let path = example.join("sqlc.yaml");
            let text = fs::read_to_string(&path).unwrap();
            let config: SqlcYaml = serde_yaml_ng::from_str(&text)
                .unwrap_or_else(|error| panic!("parsing {path:?}: {error}"));
            (config.sql.into_iter().flat_map(|package| package.codegen))
                .map(move |codegen| Case {
                    example: example.clone(),
                    name: codegen.out,
                    options: codegen.options,
                })
                .collect::<Vec<_>>()
        })
        .collect();
    assert!(!cases.is_empty(), "no cases found in {examples:?}");
    cases
}

/// The golden case named `name`.
pub fn case(name: &str) -> Case {
    (cases().into_iter())
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("no golden case {name}"))
}

impl Case {
    /// The directory holding what the case generates.
    pub fn dir(&self) -> PathBuf {
        self.example.join(&self.name)
    }

    /// The request the case generates from: what sqlc sends for its example
    /// (`tests/sqlc/examples/`, captured by `cargo xtask capture`), with its
    /// options and `out`.
    pub fn request(&self) -> plugin::GenerateRequest {
        let example = self.example.file_name().unwrap().to_string_lossy();
        let fixture = root().join(format!("tests/sqlc/examples/{example}.json"));
        let mut request: plugin::GenerateRequest = read_json(&fixture);
        request.plugin_options = serde_json::to_vec(&self.options).unwrap();
        let settings = request.settings.get_or_insert_with(Default::default);
        settings.codegen = Some(plugin::Codegen {
            out: self.name.clone(),
            plugin: "python".into(),
            ..Default::default()
        });
        request
    }
}

/// The files a golden case holds as generated, as a plugin returns them, by
/// name: all but what tools hide there (`.ruff_cache`).
pub fn generated_files(dir: &Path) -> Vec<plugin::File> {
    // A case not yet blessed has nothing generated.
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<plugin::File> = (entries)
        .map(|entry| entry.unwrap().path())
        .filter(|path| is_generated(path))
        .map(|path| plugin::File {
            name: path.file_name().unwrap().to_string_lossy().into_owned(),
            contents: fs::read(&path).unwrap(),
        })
        .collect();
    files.sort_by(|a, b| a.name.cmp(&b.name));
    files
}

/// Whether a file of a golden case is one the plugin generated.
fn is_generated(path: &Path) -> bool {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    path.is_file() && !name.starts_with('.')
}

/// `GEN_DIR`, where `cargo xtask` collects what the tests generate.
pub fn gen_dir() -> Option<PathBuf> {
    std::env::var_os("GEN_DIR").map(PathBuf::from)
}

/// Writes one generated output as the package `<GEN_DIR>/<package>/`, where
/// `cargo xtask check` runs the Python toolchain over it. Does nothing unless
/// `GEN_DIR` is set.
pub fn export(package: &str, files: &[plugin::File]) {
    let Some(dir) = gen_dir() else { return };
    for file in files {
        let path = dir.join(package).join(&file.name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &file.contents).unwrap();
    }
}
