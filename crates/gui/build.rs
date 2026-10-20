use std::path::{Path, PathBuf};
use std::process::Command;
use std::{env, fs};

const MISSING: &str = "the art is missing: run `git fetch origin assets:assets`, or check out the assets branch into target/art";

fn git(args: &[&str]) -> Option<Vec<u8>> {
    let output = Command::new("git")
        .args(["-c", "safe.directory=*"])
        .args(args)
        .output()
        .ok()?;
    output.status.success().then_some(output.stdout)
}

fn branch() -> Vec<String> {
    if git(&["rev-parse", "--verify", "--quiet", "refs/heads/assets"]).is_none() {
        let fetched = git(&["remote", "get-url", "origin"])
            .and_then(|_| git(&["fetch", "origin", "assets:assets"]));
        assert!(fetched.is_some(), "{MISSING}");
    }
    let common = git(&["rev-parse", "--git-common-dir"]).expect(MISSING);
    let common = PathBuf::from(String::from_utf8(common).expect("path").trim());
    println!(
        "cargo:rerun-if-changed={}",
        common.join("refs/heads/assets").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        common.join("packed-refs").display()
    );
    let listing = git(&[
        "ls-tree",
        "-r",
        "-z",
        "--full-tree",
        "--name-only",
        "refs/heads/assets",
    ]);
    let listing =
        String::from_utf8(listing.expect("the assets branch can't be listed")).expect("names");
    listing.split('\0').map(String::from).collect()
}

fn folder(art: &Path) -> Vec<String> {
    println!("cargo:rerun-if-changed={}", art.display());
    let mut names = Vec::new();
    for dir in ["themes", "media"] {
        let files =
            fs::read_dir(art.join(dir)).unwrap_or_else(|error| panic!("target/art/{dir}: {error}"));
        for file in files.flatten() {
            names.push(format!("{dir}/{}", file.file_name().to_string_lossy()));
        }
    }
    names
}

// ci checks the assets branch out into target/art, a local build reads the branch
fn art(out: &Path) {
    let art = Path::new("../../target/art");
    let local = art.exists();
    let names = if local { folder(art) } else { branch() };
    for name in names
        .into_iter()
        .filter(|name| name.starts_with("themes/") || name.starts_with("media/"))
    {
        let bytes = match local {
            true => fs::read(art.join(&name)).unwrap_or_else(|error| panic!("{name}: {error}")),
            false => {
                git(&["show", &format!("refs/heads/assets:{name}")]).expect("the art can't be read")
            }
        };
        let target = out.join("art").join(&name);
        fs::create_dir_all(target.parent().expect("folder")).expect("art folder");
        fs::write(target, bytes).expect("art file");
    }
}

fn main() {
    slint_build::compile("ui/app.slint").expect("ui");
    art(Path::new(&env::var("OUT_DIR").expect("out")));
    let commit = Command::new("git")
        // in a container another user owns the repository, and git then gives no commit
        .args(["-c", "safe.directory=*", "rev-parse", "--short=12", "HEAD"])
        .output();
    let commit = commit
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok());
    println!(
        "cargo:rustc-env=COMMIT={}",
        commit.as_deref().map_or("unknown", str::trim)
    );
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs/heads");
    // the system that the app is for, which is not always the system that builds it
    if std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|system| system == "windows") {
        winresource::WindowsResource::new()
            .set_icon("../../assets/interpolini.ico")
            .compile()
            .expect("icon");
    }
}
