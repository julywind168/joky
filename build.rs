use std::{env, path::PathBuf};

#[path = "build_support/version.rs"]
mod version;

fn main() {
    println!("cargo::rustc-check-cfg=cfg(joky_bootstrap)");
    println!("cargo::rerun-if-changed=src/stdlib");
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let source = version::source(&root);
    for input in version::SOURCE_INPUTS {
        println!("cargo::rerun-if-changed={input}");
    }
    for path in &source.watch {
        println!("cargo::rerun-if-changed={}", path.display());
    }
    println!("cargo::rustc-env=JOKY_BUILD_COMMIT={}", source.commit);
    println!("cargo::rustc-env=JOKY_BUILD_SOURCE_STATE={}", source.state);
    for name in ["TARGET", "PROFILE", "OPT_LEVEL"] {
        println!(
            "cargo::rustc-env=JOKY_BUILD_{name}={}",
            env::var(name).unwrap()
        );
    }
    let rustc = std::process::Command::new(env::var_os("RUSTC").unwrap())
        .arg("--version")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|text| text.lines().next().map(str::to_owned))
        .unwrap_or_else(|| "unknown".into());
    println!("cargo::rustc-env=JOKY_BUILD_RUSTC={rustc}");
    joky_stdlib_bootstrap::write_prelude_image(&PathBuf::from(env::var_os("OUT_DIR").unwrap()));
}
