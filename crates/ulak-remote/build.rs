//! Embeds the git sha as `ULAK_GIT_SHA`: `git rev-parse --short HEAD`, else env `GIT_SHA`, else "unknown".
//! (Docker builds exclude .git and pass GIT_SHA as a build arg.)
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=GIT_SHA");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    let git = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned());
    let sha = git
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::env::var("GIT_SHA")
                .ok()
                .map(|s| s.chars().take(7).collect())
        })
        .filter(|s: &String| !s.is_empty())
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=ULAK_GIT_SHA={sha}");
}
