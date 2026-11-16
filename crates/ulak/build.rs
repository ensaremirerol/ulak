//! Also embeds the git sha as `ULAK_GIT_SHA` (git, else env `GIT_SHA`, else "unknown"), like ulak-remote.
//! Rune links `-lxkbcommon`; machines with only the runtime `libxkbcommon.so.N` (no -dev package)
//! fail to link. Make a `libxkbcommon.so` symlink in OUT_DIR when none exists.
use std::path::Path;
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
                .map(|s| s.chars().take(12).collect())
        })
        .filter(|s: &String| !s.is_empty())
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=ULAK_GIT_SHA={sha}");
    println!("cargo:rerun-if-changed=build.rs");
    // cfg!() here describes the build host; the target is what matters when cross-compiling.
    #[cfg(unix)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        xkbcommon_link();
    }
}

#[cfg(unix)]
fn xkbcommon_link() {
    let dirs = [
        "/usr/lib/x86_64-linux-gnu",
        "/usr/lib64",
        "/usr/lib",
        "/lib/x86_64-linux-gnu",
        "/usr/lib/aarch64-linux-gnu",
    ];
    if dirs
        .iter()
        .any(|d| Path::new(d).join("libxkbcommon.so").exists())
    {
        return;
    }
    let Some(real) = dirs
        .iter()
        .flat_map(|d| (0..=2).map(move |n| Path::new(d).join(format!("libxkbcommon.so.{n}"))))
        .find(|p| p.exists())
    else {
        return;
    };
    let out = std::env::var("OUT_DIR").expect("cargo sets OUT_DIR for build scripts");
    let link = Path::new(&out).join("libxkbcommon.so");
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink(&real, &link).expect("symlink libxkbcommon.so in OUT_DIR");
    println!("cargo:rustc-link-search=native={out}");
}
