use std::io::Result;

fn main() -> Result<()> {
    println!("cargo:rerun-if-changed=protos/sqlc.proto");

    // `prost-build` shells out to `protoc`, so we point it at the vendored binary and a checkout builds without a
    // system-wide protobuf install.
    if std::env::var_os("PROTOC").is_none()
        && let Ok(protoc) = protoc_bin_vendored::protoc_bin_path()
    {
        // SAFETY: single-threaded build script, before any thread is spawned.
        unsafe { std::env::set_var("PROTOC", protoc) };
    }

    // sqlc dumps a `GenerateRequest` as protobuf JSON through its built-in `json` codegen (see
    // `tests/python/capture.py`) and the tests read those dumps back, so we derive `Deserialize` on the wire types. The
    // one convention serde does not share is that an absent field is the proto3 zero value, so every message defaults
    // instead of failing.
    //
    // NOTE: A `oneof` is an enum and only derives. The dumps write it under the variant's own key, which a message
    // ignores like any key it has no field for, so it reads as unset. That is fine as nothing here reads one.
    //
    // We skip the `bytes` fields, which protobuf JSON base64-encodes, as a fixture only carries the catalog (plugin
    // options belong to the test case) and decoding them would be machinery for nothing. The plugin itself never goes
    // through serde, sqlc speaks protobuf to it.
    let mut config = prost_build::Config::new();
    config
        .type_attribute(".", "#[derive(::serde::Deserialize)]")
        .message_attribute(".", "#[serde(default)]");
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
