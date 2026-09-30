//! Sets `NED_VERSION` to the package version with the git commit it's built
//! from as semver build metadata, plus `.dirty` and the build time for
//! uncommitted changes: `0.1.0+a2faeba`, `0.1.0+a2faeba.dirty.1790698892`. A
//! daemon serves only its own build (command-language spec §1.1), so every
//! build needs a distinct version.

use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("--no-optional-locks")
        .args(args)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn main() {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    // Cargo marks a `cargo install --git` checkout finished with an untracked
    // `.cargo-ok` at its root; that doesn't make the build dirty.
    let status = [
        "status",
        "--porcelain",
        "--",
        ":(top)",
        ":(top,exclude).cargo-ok",
    ];
    let build = match git(&["rev-parse", "--short", "HEAD"]) {
        Some(commit) if git(&status).is_some_and(|s| s.is_empty()) => commit,
        Some(commit) => format!("{commit}.dirty.{now}"),
        None => format!("unknown.{now}"),
    };
    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    println!("cargo:rustc-env=NED_VERSION={version}+{build}");

    println!("cargo:rerun-if-changed=../../crates");
    println!("cargo:rerun-if-changed=../../queries");
    let head = git(&["rev-parse", "--symbolic-full-name", "HEAD"]);
    for name in ["HEAD", "index", "packed-refs"]
        .into_iter()
        .chain(head.as_deref())
    {
        if let Some(path) = git(&["rev-parse", "--git-path", name])
            && Path::new(&path).exists()
        {
            println!("cargo:rerun-if-changed={path}");
        }
    }
}
