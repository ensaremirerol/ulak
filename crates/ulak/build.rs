//! Rune links `-lxkbcommon`; machines with only the runtime `libxkbcommon.so.N` (no -dev package)
//! fail to link. Make a `libxkbcommon.so` symlink in OUT_DIR when none exists.
use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if cfg!(not(target_os = "linux")) {
        return;
    }
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
