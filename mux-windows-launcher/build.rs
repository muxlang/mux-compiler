use std::{env, fs, path::PathBuf};

fn main() {
    let compiler_manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap())
        .join("../mux-compiler/Cargo.toml");
    println!("cargo:rerun-if-changed={}", compiler_manifest.display());
    let manifest = fs::read_to_string(compiler_manifest).expect("read compiler manifest");
    let package = manifest
        .split("[package]")
        .nth(1)
        .expect("compiler package metadata");
    let version = package
        .lines()
        .take_while(|line| !line.starts_with('['))
        .filter_map(|line| line.split_once('='))
        .find_map(|(name, value)| (name.trim() == "version").then_some(value.trim()))
        .and_then(|value| value.strip_prefix('"'))
        .and_then(|value| value.strip_suffix('"'))
        .expect("compiler package version");
    println!("cargo:rustc-env=MUX_COMPILER_VERSION={version}");
}
