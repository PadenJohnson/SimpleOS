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
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/index");
    println!("cargo:rerun-if-changed=src");
}
