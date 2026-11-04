use std::{env, path::PathBuf};

fn main() {
    println!("cargo::rustc-check-cfg=cfg(joky_bootstrap)");
    println!("cargo::rerun-if-changed=src/stdlib");
    joky_stdlib_bootstrap::write_prelude_image(&PathBuf::from(env::var_os("OUT_DIR").unwrap()));
}
