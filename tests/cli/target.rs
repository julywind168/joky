use super::*;

#[test]
fn unsupported_target_fails_before_reading_sources_or_replacing_artifacts() {
    let root = std::env::temp_dir().join(format!("joky-target-rejection-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let source = root.join("missing.jk");
    let output = root.join("app");
    let artifacts = [
        output.clone(),
        root.join("app.build.json"),
        root.join("app.joky.o"),
        root.join("app.joky-launcher.c"),
        root.join("app.dSYM/Contents/Resources/DWARF/app"),
    ];
    for artifact in &artifacts {
        fs::create_dir_all(artifact.parent().unwrap()).unwrap();
        fs::write(artifact, "previous artifact").unwrap();
    }
    let foreign = if env!("JOKY_TARGET") == "x86_64-unknown-linux-gnu" {
        "aarch64-apple-darwin"
    } else {
        "x86_64-unknown-linux-gnu"
    };
    for target in [foreign, "not-a-target", ""] {
        let result = Command::new(env!("CARGO_BIN_EXE_joky"))
            .arg("build")
            .arg(&source)
            .args(["--target", target, "-o"])
            .arg(&output)
            .env("JOKY_RUNTIME_ARCHIVE", root.join("missing-runtime.a"))
            .output()
            .unwrap();
        assert_eq!(result.status.code(), Some(1));
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(
            stderr.contains(&format!("unsupported AOT target '{target}'")),
            "{stderr}"
        );
        assert!(stderr.contains(env!("JOKY_TARGET")), "{stderr}");
        assert!(!stderr.contains("Building"), "{stderr}");
        for artifact in &artifacts {
            assert_eq!(fs::read_to_string(artifact).unwrap(), "previous artifact");
        }
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn target_option_requires_one_value_and_is_build_only() {
    for arguments in [
        vec!["build", "--target"],
        vec![
            "build",
            "--target",
            env!("JOKY_TARGET"),
            "--target",
            env!("JOKY_TARGET"),
        ],
        vec!["run", "--target", env!("JOKY_TARGET")],
    ] {
        let result = Command::new(env!("CARGO_BIN_EXE_joky"))
            .args(arguments)
            .output()
            .unwrap();
        assert_eq!(result.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&result.stderr).contains("usage:"));
    }
}
