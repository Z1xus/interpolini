use std::process::Command;

fn main() {
    slint_build::compile("ui/app.slint").expect("ui");
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
