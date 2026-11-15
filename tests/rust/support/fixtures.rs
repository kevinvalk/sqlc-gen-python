use std::fs;
use std::path::{Path, PathBuf};

use sqlc_gen_python::plugin;

/// We ask cargo for the root of this run rather than of the build, as a `target/` reused by another copy of the tree
/// would otherwise test the first copy's fixtures.
pub fn root() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("run by cargo"))
}

pub fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> T {
    let contents = fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("reading {path:?}: {error} (fixtures come from `cargo xtask capture`)"));
    serde_json::from_str(&contents).unwrap_or_else(|error| panic!("parsing {path:?}: {error}"))
}

/// A golden case is one `codegen` entry of an example's `sqlc.yaml`: `name` is its `out`, the directory beside the
/// `sqlc.yaml` that holds what it generates, and `options` are what sqlc hands the plugin.
pub struct Case {
    pub example: PathBuf,
    pub name: String,
    pub options: serde_json::Value,
}

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

pub fn cases() -> Vec<Case> {
    let examples_directory = root().join("examples");
    let mut examples: Vec<PathBuf> = (fs::read_dir(&examples_directory).unwrap())
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.join("sqlc.yaml").is_file())
        .collect();
    examples.sort();

    let cases: Vec<Case> = (examples.into_iter())
        .flat_map(|example| {
            let path = example.join("sqlc.yaml");
            let yaml = fs::read_to_string(&path).unwrap();
            let sqlc_yaml: SqlcYaml =
                serde_yaml_ng::from_str(&yaml).unwrap_or_else(|error| panic!("parsing {path:?}: {error}"));
            (sqlc_yaml.sql.into_iter().flat_map(|package| package.codegen))
                .map(move |codegen| Case {
                    example: example.clone(),
                    name: codegen.out,
                    options: codegen.options,
                })
                .collect::<Vec<_>>()
        })
        .collect();
    assert!(!cases.is_empty(), "no cases found in {examples_directory:?}");
    cases
}

pub fn case(name: &str) -> Case {
    (cases().into_iter())
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("no golden case {name}"))
}

impl Case {
    pub fn directory(&self) -> PathBuf {
        self.example.join(&self.name)
    }

    /// What sqlc sends for the case's example (`tests/sqlc/examples/`, from `cargo xtask capture`), with the case's own
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

/// The files a golden case holds, by name, as a plugin returns them.
pub fn generated_files(directory: &Path) -> Vec<plugin::File> {
    // A case that was never blessed has no directory yet.
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };

    // NOTE: We skip dot files as tools hide their own there (`.ruff_cache`). Keep this in sync with `mirror_generated`
    // in xtask, which blesses a case.
    let mut files: Vec<plugin::File> = (entries)
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_file() && !path.file_name().unwrap_or_default().to_string_lossy().starts_with('.'))
        .map(|path| plugin::File {
            name: path.file_name().unwrap().to_string_lossy().into_owned(),
            contents: fs::read(&path).unwrap(),
        })
        .collect();
    files.sort_by(|a, b| a.name.cmp(&b.name));
    files
}

/// `GEN_DIR`, where `cargo xtask` collects what the tests generate.
pub fn gen_dir() -> Option<PathBuf> {
    std::env::var_os("GEN_DIR").map(PathBuf::from)
}

/// Writes one output as the package `<GEN_DIR>/<package>/`, for `cargo xtask check` to run the Python toolchain over.
/// Without `GEN_DIR` we write nothing.
pub fn export(package: &str, files: &[plugin::File]) {
    let Some(directory) = gen_dir() else { return };
    for file in files {
        let path = directory.join(package).join(&file.name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &file.contents).unwrap();
    }
}
