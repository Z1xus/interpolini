use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let config = slint_build::CompilerConfiguration::new().with_library_paths(HashMap::from([(
        "app".to_string(),
        PathBuf::from("../../crates/gui/ui"),
    )]));
    slint_build::compile_with_config("window.slint", config).expect("ui");
    let manifest = fs::read_to_string("../../Cargo.toml").expect("manifest");
    let version = manifest
        .lines()
        .find_map(|line| line.strip_prefix("version = "))
        .expect("version")
        .trim_matches('"');
    let commit = Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok());
    let commit = commit.as_deref().map_or("unknown", str::trim);
    println!("cargo:rustc-env=VERSION={version} ({commit})");
    println!("cargo:rerun-if-changed=../../Cargo.toml");
    println!("cargo:rerun-if-changed=../../crates/gui/ui");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
}
