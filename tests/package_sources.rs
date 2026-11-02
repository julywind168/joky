//! Embedding graph validation, stable identities and detached source snapshots.
#[path = "support/scratch.rs"]
mod scratch;
use joky::{Checker, PackageSources, SourcePackage};
use std::{collections::BTreeMap, fs, path::Path};

fn package(name: &str, root: &Path) -> SourcePackage {
    SourcePackage {
        name: name.into(),
        source_root: root.into(),
        dependencies: BTreeMap::new(),
    }
}

#[test]
fn package_sources_preserve_names_across_aliases_order_and_checkout_paths() {
    let work = scratch::ScratchDir::new("package-identities");
    let checker = Checker::new();
    let mut previous_name = None;
    let mut snapshot = None;
    for (checkout, alias, index) in [("first", "dep", 1), ("second", "renamed", 2)] {
        let root = work.path().join(checkout);
        for directory in ["app", "common", "spare"] {
            fs::create_dir_all(root.join(directory)).unwrap();
        }
        let entry = root.join("app/main.jk");
        fs::write(&entry, format!("import {alias}/model fn answer() -> Int32 {{ let item = model.make(); item.value }}")).unwrap();
        fs::write(
            root.join("common/model.jk"),
            "pub struct Item { pub value: Int32 } pub fn make() -> Item { Item { value: 42 } }",
        )
        .unwrap();
        let mut app = package("app", &root.join("app"));
        app.dependencies.insert(alias.into(), index);
        let common = package("common-lib", &root.join("common"));
        let spare = package("spare", &root.join("spare"));
        let packages = if index == 1 {
            vec![app, common, spare]
        } else {
            vec![app, spare, common]
        };
        let graph = PackageSources::new(packages).unwrap();
        let ast = graph.resolve_file(&entry, checker.type_registry()).unwrap();
        checker.check_module(&ast).unwrap();
        let names: Vec<_> = ast
            .records
            .iter()
            .filter(|record| record.name.ends_with(".Item"))
            .map(|record| record.name.clone())
            .collect();
        assert_eq!(names.len(), 1);
        if let Some(previous) = previous_name {
            assert_eq!(names[0], previous);
        }
        previous_name = Some(names[0].clone());
        snapshot = Some(ast);
    }
    drop(work);
    let ast = snapshot.unwrap();
    let mut vm = joky::vm::VmEngine::with_mode(joky::vm::VmMode::Interpreter);
    vm.load_module(&ast).unwrap();
    assert_eq!(vm.call_int32_0("answer").unwrap(), 42);
    assert!(ast
        .sources()
        .files()
        .any(|source| source.name().ends_with("common/model.jk")
            && source.text().contains("value: 42")));
}

#[test]
fn package_sources_validate_public_graph_inputs_before_traversal() {
    let work = scratch::ScratchDir::new("package-inputs");
    let a = work.path().join("a");
    let b = work.path().join("b");
    fs::create_dir(&a).unwrap();
    fs::create_dir(&b).unwrap();
    assert!(PackageSources::new(vec![]).is_err());
    assert!(PackageSources::new(vec![package("a", &a), package("a", &b)]).is_err());
    assert!(PackageSources::new(vec![package("a", &a), package("b", &a)]).is_err());
    for (alias, target) in [("joky", 1), ("bad/name", 1), ("dep", 2), ("self", 0)] {
        let mut root = package("a", &a);
        root.dependencies.insert(alias.into(), target);
        assert!(PackageSources::new(vec![root, package("b", &b)]).is_err());
    }
    let mut root = package("a", &a);
    root.dependencies.insert("b".into(), 1);
    let mut dependency = package("b", &b);
    dependency.dependencies.insert("a".into(), 0);
    assert!(PackageSources::new(vec![root, dependency]).is_err());
}
