use std::io::Result;

fn main() -> Result<()> {
    println!("cargo:rerun-if-changed=protos/sqlc.proto");

    // `prost-build` shells out to `protoc`; point it at the vendored binary so a
    // checkout builds without a system-wide protobuf install.
    if std::env::var_os("PROTOC").is_none()
        && let Ok(protoc) = protoc_bin_vendored::protoc_bin_path()
    {
        // SAFETY: single-threaded build script, before any thread is spawned.
        unsafe { std::env::set_var("PROTOC", protoc) };
    }

    // sqlc dumps a `GenerateRequest` as protobuf JSON via its built-in `json`
    // codegen (see `tests/python/capture.py`), and the tests read those dumps back.
    // Deriving `Deserialize` on the wire types lets them load directly, with the
    // one convention serde does not share handled here: absent fields fall back
    // to the proto3 zero value rather than failing.
    //
    // The `bytes` fields are skipped. protobuf JSON base64-encodes them, but a
    // fixture carries only the catalog — plugin options belong to the test case,
    // not the schema — so decoding them would be machinery for nothing. The
    // plugin itself never goes through serde; sqlc speaks protobuf.
    let mut config = prost_build::Config::new();
    config
        .type_attribute(".", "#[derive(::serde::Deserialize)]")
        .type_attribute(".", "#[serde(default)]");
    for bytes_field in [
        "plugin.File.contents",
        "plugin.Codegen.options",
        "plugin.GenerateRequest.plugin_options",
        "plugin.GenerateRequest.global_options",
    ] {
        config.field_attribute(bytes_field, "#[serde(skip)]");
    }
    config.compile_protos(&["protos/sqlc.proto"], &["protos/"])
}
