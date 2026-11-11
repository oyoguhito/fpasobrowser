use std::{env, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/index");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");

    let commit = Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| String::from("unknown"));

    let dirty = Command::new("git")
        .args(["status", "--short"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| !output.stdout.is_empty())
        .unwrap_or(false);

    let build_date = env::var("SOURCE_DATE_EPOCH").unwrap_or_else(|_| String::from("local"));

    println!("cargo:rustc-env=FPASOBROWSER_GIT_COMMIT={commit}");
    println!(
        "cargo:rustc-env=FPASOBROWSER_GIT_DIRTY={}",
        if dirty { "dirty" } else { "clean" }
    );
    println!("cargo:rustc-env=FPASOBROWSER_BUILD_DATE={build_date}");
}
