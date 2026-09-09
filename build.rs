//! Compiles the vendored HAPI protobufs (proto/) into Rust with tonic server stubs.
//! Uses the vendored protoc so a system protobuf install is not required.

use std::path::{Path, PathBuf};

fn collect(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "proto") {
            out.push(path);
        }
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    // SAFETY: build scripts run single-threaded before any spawned thread reads the environment.
    unsafe { std::env::set_var("PROTOC", &protoc) };
    let well_known = protoc_bin_vendored::include_path()?;

    let root = PathBuf::from("proto");
    let mut files = Vec::new();
    collect(&root, &mut files)?;
    files.sort();

    // Every import in the vendored tree is written relative to proto/ ("services/x.proto",
    // "platform/…"), so a single include root is enough and nothing is shadowed.
    tonic_prost_build::configure()
        .build_client(true)
        .build_server(true)
        .include_file("hapi.rs")
        .compile_protos(&files, &[root.clone(), well_known])?;

    println!("cargo:rerun-if-changed=proto");
    Ok(())
}
