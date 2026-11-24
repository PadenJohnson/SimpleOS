//! Build script: links the kernel with `linker.ld`, and stamps it with the commit it was built
//! from and that commit's time (shown on the crash screen and used to tell which of two Solon
//! versions is newer when updating from a USB drive).

use std::env;
use std::process::Command;

fn main() {
    let dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    println!("cargo:rustc-link-arg=-T{dir}/linker.ld");
    println!("cargo:rerun-if-changed=linker.ld");
    // Which commit this kernel was built from (shown on the crash screen, so a photo of it
    // can be matched to the right kernel to look up addresses with `mkimg sym`).
    let commit = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| String::from("unknown"));
    let dirty = Command::new("git").args(["status", "--porcelain", "--untracked-files=no"]).output().map(|o| !o.stdout.is_empty()).unwrap_or(false);
    println!("cargo:rustc-env=SOLON_BUILD={}{}", commit, if dirty { "+" } else { "" });
    let time = Command::new("git").args(["log", "-1", "--format=%ct"]).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).filter(|s| !s.is_empty()).unwrap_or_else(|| String::from("0"));
    println!("cargo:rustc-env=SOLON_BUILD_TIME={}", time);
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/index");
    println!("cargo:rerun-if-changed=src");
}
