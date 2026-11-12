use std::process::Command;

// Compile time checks.
fn main() {
    println!("cargo:rerun-if-changed=src/frontend/router/sharding/hashfn.c");

    cc::Build::new()
        .file("src/frontend/router/sharding/hashfn.c")
        .flags(["-Wno-implicit-fallthrough"])
        .compile("postgres_hash");

    println!("cargo:rerun-if-env-changed=GIT_HASH");
    // Every commit and checkout appends to it: keeps GIT_HASH current.
    println!("cargo:rerun-if-changed=../.git/logs/HEAD");

    // "unknown" outside a git checkout, e.g. a source tarball.
    let git_hash = std::env::var("GIT_HASH").unwrap_or_else(|_| {
        Command::new("git")
            .args(["rev-parse", "--short=7", "HEAD"])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| String::from_utf8(output.stdout).ok())
            .map(|hash| hash.trim().to_owned())
            .filter(|hash| !hash.is_empty())
            .unwrap_or_else(|| "unknown".into())
    });

    println!("cargo:rustc-env=GIT_HASH={git_hash}");
}
