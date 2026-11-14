use super::*;
use crate::cli::scratch;
use std::fs;

#[test]
fn lsp_checks_only_invalidated_projects_and_prioritizes_navigation() {
    let work = scratch::ScratchDir::new("lsp-projects");
    let root = work.path().canonicalize().unwrap();
    for name in ["a", "b", "independent"] {
        fs::create_dir(root.join(name)).unwrap();
        let mut manifest = format!("[project]\nname='{name}'\nentry='main.jk'");
        if name != "independent" {
            manifest.push_str("\n[dependencies]\nshared={path='../shared'}");
        }
        fs::write(root.join(name).join("joky.toml"), manifest).unwrap();
        let source = if name == "independent" {
            "fn main() -> Int32 { 42 }"
        } else {
            "import shared/value\nfn main() -> Int32 { value.answer() }"
        };
        fs::write(root.join(name).join("main.jk"), source).unwrap();
    }
    fs::create_dir(root.join("shared")).unwrap();
    fs::write(root.join("shared/joky.toml"), "[package]\nname='shared'").unwrap();
    let shared = root.join("shared/value.jk");
    fs::write(&shared, "pub fn answer() -> Int32 { 42 }").unwrap();

    let checker = joky::Checker::new();
    let cache = EditorParseCache::default();
    let mut session = Session {
        roots: vec![root.clone()],
        ..Session::default()
    };
    let mut out = Vec::new();
    for name in ["a", "b", "independent"] {
        let path = root.join(name).join("main.jk");
        let uri: String = Url::from_file_path(&path).unwrap().into();
        session.documents.insert(
            uri,
            Document {
                version: 1,
                text: fs::read_to_string(&path).unwrap().into(),
                path,
            },
        );
    }
    let mut drain_checks = |session: &mut Session| {
        let mut checked = Vec::new();
        while let Some(Work::Check(snapshot, target)) = session.next_work() {
            let result = analyze(&snapshot, &target, &checker, &cache, || false).unwrap();
            checked.push(target.clone());
            session.accept(target, result, &mut out).unwrap();
        }
        checked
    };
    session.refresh(vec![], None, &mut Vec::new()).unwrap();
    assert_eq!(drain_checks(&mut session).len(), 3);

    let independent = root.join("independent/main.jk");
    let independent_uri: String = Url::from_file_path(&independent).unwrap().into();
    let doc = session.documents.get_mut(&independent_uri).unwrap();
    doc.version = 2;
    doc.text = "fn main() -> Int32 { 43 }".into();
    session
        .refresh(vec![independent], None, &mut Vec::new())
        .unwrap();
    assert_eq!(
        drain_checks(&mut session),
        [root.join("independent/joky.toml")]
    );

    let shared_uri: String = Url::from_file_path(&shared).unwrap().into();
    session.documents.insert(
        shared_uri.clone(),
        Document {
            path: shared.clone(),
            version: 1,
            text: "pub fn answer() -> Bool { true }".into(),
        },
    );
    session
        .refresh(vec![shared.clone()], None, &mut Vec::new())
        .unwrap();
    let b = root.join("b/main.jk");
    let b_uri: String = Url::from_file_path(&b).unwrap().into();
    session
        .query(
            &json!(1),
            QueryKind::Hover,
            &json!({
                "textDocument": {"uri": b_uri}, "position": {"line": 1, "character": 27}
            }),
            &mut Vec::new(),
        )
        .unwrap();
    assert_eq!(
        drain_checks(&mut session),
        [
            root.join("b/joky.toml"),
            root.join("a/joky.toml"),
            root.join("shared/joky.toml")
        ]
    );
    for name in ["a", "b"] {
        let analysis = &session.analyses[&root.join(name).join("joky.toml")];
        assert!(analysis.inputs.contains(&shared));
        assert!(analysis.inputs.contains(&root.join("shared/joky.toml")));
        assert_eq!(
            analysis.reports.values().next().unwrap().diagnostic["code"],
            "type_mismatch"
        );
    }

    // Closing the buffer must make both callers see the disk signature again.
    session.documents.remove(&shared_uri);
    session
        .refresh(vec![shared.clone()], None, &mut Vec::new())
        .unwrap();
    assert_eq!(
        drain_checks(&mut session),
        [root.join("a/joky.toml"), root.join("b/joky.toml")]
    );
    assert!(session.reports().is_empty());

    // A failed import still observes the path that may be created later.
    fs::remove_file(&shared).unwrap();
    session
        .refresh(vec![shared.clone()], None, &mut Vec::new())
        .unwrap();
    assert_eq!(drain_checks(&mut session).len(), 2);
    assert_eq!(session.reports().len(), 2);
    fs::write(&shared, "// shifted\npub fn answer() -> Int32 { 43 }").unwrap();
    session
        .refresh(vec![shared], None, &mut Vec::new())
        .unwrap();
    assert_eq!(drain_checks(&mut session).len(), 2);
    assert!(session.reports().is_empty());
}
