use super::parity::Package;

#[test]
fn saslprep_rfc_unicode_postgres_and_resource_boundaries() {
    Package::new(
        "saslprep-boundaries",
        &[("main.jk", include_str!("../fixtures/saslprep.jk"))],
    )
    .check_cached("saslprep ok\n", None, &[]);
}

#[test]
fn saslprep_independent_normalization_and_postgres_corpus() {
    // Python 3.2 UCD/stringprep and PostgreSQL 14.17 pg_saslprep oracles.
    // Provenance and regeneration command: docs/stdlib/saslprep.md.
    let package = Package::new("saslprep-differential", &[]);
    let vectors = package.root.join("vectors.bin");
    std::fs::write(
        &vectors,
        include_bytes!("../fixtures/unicode32_vectors.bin"),
    )
    .unwrap();
    let location = vectors
        .to_str()
        .unwrap()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let source =
        include_str!("../fixtures/unicode32_conformance.jk").replace("@VECTORS@", &location);
    std::fs::write(package.root.join("src/main.jk"), source).unwrap();
    package.check_cached("unicode conformance 500 512 512\n", None, &[]);
}
