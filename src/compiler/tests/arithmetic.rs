use super::*;

#[test]
fn arithmetic_bitwise_literals_assignment_and_numeric_methods() {
    run_program(include_str!("../../../tests/fixtures/arithmetic.jk"));
}

#[test]
fn string_split_replace_byte_and_slice() {
    run_program(include_str!("../../../tests/fixtures/string_slice.jk"));
}

#[test]
fn list_map_filter_and_fold() {
    let root = std::env::temp_dir().join(format!("joky-list-{}", std::process::id()));
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/main.jk"),
        include_str!("../../../tests/fixtures/list_combinators.jk"),
    )
    .unwrap();
    let graph = crate::module::ModuleGraph::load(&root.join("src/main.jk"), &root).unwrap();
    let mut compiler = Compiler::new().unwrap();
    let result = compiler.run_modular_program(&graph, &root.join("cache"));
    std::fs::remove_dir_all(&root).unwrap();
    result.expect("test program should run");
}
