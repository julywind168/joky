//! Source-preserving fixture conversion shared by VM and native backend tests.
use super::{ast::*, lexer::TokenKind, parse, Lexer};

#[derive(Clone, Copy)]
pub(crate) enum Suspends {
    All,
    /// Only declarations with an explicit return annotation, including Unit.
    Typed,
    Named(&'static str),
}

fn functions(module: &Module) -> impl Iterator<Item = &FnDef> {
    module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::FnDef(function) => Some(function),
            Item::ExternFn(_) => None,
        })
        .chain(module.implementations.iter().flat_map(|item| &item.methods))
}

/// Use parsed declarations to select functions and balanced tokens to locate
/// their parameter lists. Preserve source text, extern ABIs and existing
/// attributes; verify the selected declarations after inserting new attributes.
pub(crate) fn with_suspends(source: &str, suspends: bool, which: Suspends) -> String {
    if !suspends {
        return source.to_owned();
    }
    let module = parse(source).expect("valid source fixture");
    let tokens = Lexer::tokenize(source).expect("valid fixture tokens");
    let mut insertions = Vec::new();
    let mut expected = Vec::new();
    let mut selected_count = 0;
    for function in functions(&module) {
        let start = tokens.partition_point(|token| token.span.start() < function.span.start());
        let end = tokens.partition_point(|token| token.span.start() < function.body.span().start());
        let header = &tokens[start..end];
        let selected = match which {
            Suspends::All => true,
            Suspends::Typed => header.iter().any(|token| token.kind == TokenKind::Arrow),
            Suspends::Named(name) => function.name == name,
        };
        selected_count += usize::from(selected);
        expected.push((function.name.clone(), function.suspends || selected));
        if !selected || function.suspends {
            continue;
        }
        let open = header
            .iter()
            .position(|token| token.kind == TokenKind::LParen)
            .expect("function parameter list");
        let mut depth = 0;
        let close = header[open..]
            .iter()
            .find(|token| {
                match token.kind {
                    TokenKind::LParen => depth += 1,
                    TokenKind::RParen => depth -= 1,
                    _ => {}
                }
                depth == 0
            })
            .expect("balanced function parameters");
        insertions.push(close.span.end());
    }
    if let Suspends::Named(name) = which {
        assert!(selected_count > 0, "fixture has no function named {name}");
    }
    insertions.sort_unstable();
    let mut result = source.to_owned();
    for offset in insertions.into_iter().rev() {
        result.insert_str(offset, " @suspends");
    }
    let rewritten = parse(&result).expect("valid suspending fixture");
    assert_eq!(
        functions(&rewritten)
            .map(|function| (function.name.clone(), function.suspends))
            .collect::<Vec<_>>(),
        expected,
        "fixture conversion must mark exactly the selected declarations",
    );
    result
}

#[test]
fn fixture_conversion_handles_declarations_without_rewriting_source_contents() {
    let source = r#"// λ fn comment() -> Int32 { 0 }
extern fn host(pair: (Int32, Int32)) -> Int32
pub fn generic<T>(
    pair: (T, (Int32, Bool)),
) // A comment containing ) -> and @suspends.
-> (T, Int32) { let (value, (number, _)) = pair (value, number) }
fn implicit() {} fn explicit() -> Unit {}
fn ready() @suspends -> Int32 { 42 }
fn text() -> String { "fn fake() -> Int32 {{ 0 }}" }
struct Point { x: Int32 }
impl Point {
    pub fn method(self, pair: (Int32, Int32)) -> Int32 { self.x }
    fn unit(self) {}
}
"#;
    for (which, expected) in [
        (
            Suspends::All,
            vec![true, true, true, true, true, true, true],
        ),
        (
            Suspends::Typed,
            vec![true, false, true, true, true, true, false],
        ),
        (
            Suspends::Named("generic"),
            vec![true, false, false, true, false, false, false],
        ),
    ] {
        let rewritten = with_suspends(source, true, which);
        let module = parse(&rewritten).unwrap();
        let actual: Vec<_> = functions(&module)
            .map(|function| function.suspends)
            .collect();
        assert_eq!(actual, expected);
        assert!(matches!(&module.items[0], Item::ExternFn(function) if !function.suspends));
        assert!(rewritten.contains("// λ fn comment() -> Int32 { 0 }"));
        assert!(rewritten.contains("// A comment containing ) -> and @suspends."));
        assert!(rewritten.contains("\"fn fake() -> Int32 {{ 0 }}\""));
        assert_eq!(with_suspends(&rewritten, true, which), rewritten);
        assert_eq!(with_suspends(source, false, which), source);
    }
}

#[test]
#[should_panic(expected = "fixture has no function named missing")]
fn fixture_conversion_rejects_missing_named_targets() {
    with_suspends("fn present() {}", true, Suspends::Named("missing"));
}
