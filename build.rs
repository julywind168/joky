use std::path::Path;

fn hash_sources(path: &Path, hash: &mut u64) {
    if path.is_dir() {
        let mut files = std::fs::read_dir(path)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        files.sort();
        for file in files {
            hash_sources(&file, hash);
        }
    } else {
        for byte in path
            .to_string_lossy()
            .bytes()
            .chain(std::fs::read(path).unwrap())
        {
            *hash = (*hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
        }
    }
}

fn main() {
    let mut hash = 0xcbf29ce484222325;
    for path in [
        "src",
        "Cargo.toml",
        "Cargo.lock",
        "build.rs",
        "std/joky/list.jk",
        "std/joky/mut_list.jk",
        "std/joky/option.jk",
        "std/joky/result.jk",
        "std/joky/bytes.jk",
        "std/joky/string.jk",
        "std/joky/map.jk",
        "std/joky/mut_bytes.jk",
        "std/joky/mut_map.jk",
        "std/joky/mut_set.jk",
    ] {
        println!("cargo:rerun-if-changed={path}");
        hash_sources(Path::new(path), &mut hash);
    }
    println!("cargo:rustc-env=JOKY_BUILD_ID={hash:016x}");
    println!(
        "cargo:rustc-env=JOKY_TARGET={}",
        std::env::var("TARGET").unwrap()
    );
}
