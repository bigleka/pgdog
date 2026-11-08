use std::process::Command;

// Compile time checks.
fn main() {
    println!("cargo:rerun-if-changed=src/frontend/router/sharding/hashfn.c");
    // Every commit and checkout appends to it: keeps GIT_HASH current.
    println!("cargo:rerun-if-changed=../.git/logs/HEAD");

    cc::Build::new()
        .file("src/frontend/router/sharding/hashfn.c")
        .compile("postgres_hash");

    // Empty outside a git checkout, e.g. a source tarball.
    let git_hash = Command::new("git")
        .args(["rev-parse", "--short=7", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|hash| hash.trim().to_owned())
        .filter(|hash| !hash.is_empty())
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=GIT_HASH={git_hash}");
}
