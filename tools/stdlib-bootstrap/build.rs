fn main() {
    println!("cargo::rustc-check-cfg=cfg(joky_bootstrap)");
    println!("cargo::rustc-cfg=joky_bootstrap");
}
