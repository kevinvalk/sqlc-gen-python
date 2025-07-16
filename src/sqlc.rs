pub(crate) mod plugin {
    include!(concat!(env!("OUT_DIR"), "/plugin.rs"));
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(default)]
pub(crate) struct Config {
    pub output: String,
    pub debug: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            output: String::from("queries.py"),
            debug: false,
        }
    }
}

impl Config {
    pub(crate) fn from_option(buf: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(buf)
    }
}
