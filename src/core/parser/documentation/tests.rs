use super::*;

#[test]
fn documentation_preserves_signatures_and_attaches_comments() {
    let source = r#"//! Module overview.
//! Unicode 文档.
/* /// Not documentation. /* nested */ */
/// Maps a value.
/// Keeps `T` intact.
pub fn map<T, F: FnOnce(T) -> T>(value: T, callback: F) @noescape(callback) -> T {
    callback(value)
}
/// A documented record.
#derive(Clone)
pub struct Record {
    /// Public data.
    pub value: Int32,
    hidden: Int32,
}
/// A documented enumeration.
#derive(Clone)
pub enum Choice { None, Some(Int32) }
pub trait Source {
    /// The output type.
    type Output: Clone + Show
    /// Produces a value.
    fn next(&self) @noescape -> Self.Output
}
impl Source for Record {
    /// The concrete output.
    type Output = Int32
    fn next(&self) @noescape -> Int32 { self.value }
}
/// Compiler metadata is omitted.
#intrinsic("mem.drop")
pub fn drop<T>(value: T)
/// Not attached through ordinary comments.
// Separate comment.
pub const TEXT: String = "/// not a comment  <script>"
fn private() {}
#test
fn test_case() {}
//! Not a module header.
"#;
    let docs = extract(source, false).unwrap();
    assert_eq!(docs.docs, "Module overview.\nUnicode 文档.");
    let get = |name: &str| docs.entries.iter().find(|e| e.name == name).unwrap();
    assert_eq!(
        get("map").signature,
        "pub fn map<T, F: FnOnce(T) -> T>(value: T, callback: F) @noescape(callback) -> T"
    );
    assert_eq!(get("map").docs, "Maps a value.\nKeeps `T` intact.");
    let record = get("Record");
    assert_eq!(record.docs, "A documented record.");
    assert_eq!(record.signature, "#derive(Clone) pub struct Record");
    assert_eq!(record.children.len(), 1);
    assert_eq!(record.children[0].docs, "Public data.");
    assert_eq!(get("Choice").docs, "A documented enumeration.");
    let source = get("Source");
    assert_eq!(source.children[0].signature, "type Output: Clone + Show");
    assert_eq!(source.children[0].docs, "The output type.");
    assert_eq!(
        source.children[1].signature,
        "fn next(&self) @noescape -> Self.Output"
    );
    let implementation = docs.entries.iter().find(|e| e.kind == "impl").unwrap();
    assert_eq!(
        implementation.children.last().unwrap().signature,
        "type Output = Int32"
    );
    assert_eq!(
        implementation.children.last().unwrap().docs,
        "The concrete output."
    );
    assert_eq!(get("drop").signature, "pub fn drop<T>(value: T)");
    assert_eq!(get("drop").docs, "Compiler metadata is omitted.");
    assert_eq!(
        get("TEXT").signature,
        "pub const TEXT: String = \"/// not a comment  <script>\""
    );
    assert!(get("TEXT").docs.is_empty());
    assert!(!docs
        .entries
        .iter()
        .any(|e| matches!(e.name.as_str(), "private" | "test_case")));
}

#[test]
fn documentation_uses_visibility_and_ignores_synthetic_imports() {
    let source = "import joky/option\nimport joky/internal/buffer\nfn helper() {}\nextern fn __host()\nstruct Hidden {}\nimpl Hidden { pub fn new() -> Self { Hidden {} } }\npub fn values() { let a = [1, 2] }";
    let ordinary = extract(source, false).unwrap();
    assert_eq!(ordinary.entries.len(), 1);
    assert_eq!(ordinary.imports, ["import joky/option"]);
    let embedded = extract(source, true).unwrap();
    assert!(embedded.entries.iter().any(|e| e.name == "helper"));
    assert!(!embedded.entries.iter().any(|e| e.name == "__host"));
    assert!(extract("pub fn broken(", false).is_err());
    let empty = extract("//! Only documentation.\r\n", false).unwrap();
    assert_eq!(empty.docs, "Only documentation.");
    assert!(empty.entries.is_empty());
}

#[test]
fn documentation_merges_repeated_impl_blocks() {
    let docs = extract(
        "pub struct Value {}\nimpl Value { pub fn a() {} }\nimpl Value { pub fn b() {} }",
        false,
    )
    .unwrap();
    let implementations: Vec<_> = docs.entries.iter().filter(|e| e.kind == "impl").collect();
    assert_eq!(implementations.len(), 1);
    assert_eq!(
        implementations[0]
            .children
            .iter()
            .map(|e| e.name.as_str())
            .collect::<Vec<_>>(),
        ["a", "b"]
    );
}
