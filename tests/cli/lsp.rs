use super::Workspace;
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Read, Write},
    process::{Child, ChildStdin, Stdio},
    sync::mpsc,
    time::Duration,
};
use url::Url;

struct Client {
    child: Child,
    input: ChildStdin,
    messages: mpsc::Receiver<Value>,
}

impl Client {
    fn start(work: &Workspace) -> Self {
        let mut child = work
            .command()
            .arg("lsp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap());
        let (send, messages) = mpsc::channel();
        std::thread::spawn(move || {
            let mut header = String::new();
            while output.read_line(&mut header).unwrap() != 0 {
                let length = header
                    .strip_prefix("Content-Length: ")
                    .unwrap()
                    .trim()
                    .parse()
                    .unwrap();
                header.clear();
                output.read_line(&mut header).unwrap();
                assert_eq!(header, "\r\n");
                let mut body = vec![0; length];
                output.read_exact(&mut body).unwrap();
                if send.send(serde_json::from_slice(&body).unwrap()).is_err() {
                    break;
                }
                header.clear();
            }
        });
        Self {
            child,
            input,
            messages,
        }
    }

    fn send(&mut self, mut message: Value) {
        message["jsonrpc"] = json!("2.0");
        let body = serde_json::to_vec(&message).unwrap();
        write!(self.input, "Content-Length: {}\r\n\r\n", body.len()).unwrap();
        self.input.write_all(&body).unwrap();
        self.input.flush().unwrap();
    }

    fn until(&self, predicate: impl Fn(&Value) -> bool) -> Value {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let message = self
                .messages
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .unwrap();
            if predicate(&message) {
                return message;
            }
        }
    }

    fn report(&self, uri: &Url, version: u64, revision: u64, count: usize) -> Value {
        self.until(|message| {
            let params = &message["params"];
            message["method"] == "textDocument/publishDiagnostics"
                && params["uri"] == uri.as_str()
                && params["version"] == version
                && params["jokySnapshotRevision"] == revision
                && params["diagnostics"]
                    .as_array()
                    .is_some_and(|items| items.len() == count)
        })
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn symbol_range(source: &str, line: usize, token: &str, last: bool) -> Value {
    let text = source
        .lines()
        .nth(line)
        .unwrap()
        .trim_start_matches('\u{feff}');
    let byte = if last {
        text.rfind(token)
    } else {
        text.find(token)
    }
    .unwrap();
    let character = text[..byte].encode_utf16().count();
    json!({
        "start": {"line": line, "character": character},
        "end": {"line": line, "character": character + token.encode_utf16().count()},
    })
}

#[test]
fn lsp_completes_incomplete_calls_and_module_paths_from_unsaved_sources() {
    let work = Workspace::new();
    work.write(
        "app/joky.toml",
        "[project]\nname='app'\nentry='main.jk'\n[dependencies]\nlib={path='../lib'}",
    );
    work.write("lib/joky.toml", "[package]\nname='lib'");
    work.write("lib/value.jk", "pub fn saved() -> Int32 { 1 }");
    work.write(
        "lib/helpers.jk",
        "pub fn calculate(value: Int32) -> Int32 { value }",
    );
    work.write("app/main.jk", "fn main() {}");
    let uri = |relative: &str| Url::from_file_path(work.path().join(relative)).unwrap();
    let entry = uri("app/main.jk");
    let dependency = uri("lib/value.jk");
    let mut client = Client::start(&work);
    client.send(json!({"id": 1, "method": "initialize", "params": {"rootUri": uri("").as_str()}}));
    assert_eq!(
        client.until(|msg| msg["id"] == 1)["result"]["capabilities"]["completionProvider"]
            ["triggerCharacters"],
        json!(["."])
    );
    let original = concat!(
        "import lib/value as values\n",
        "import lib/helpers.{calculate as convert}\n",
        "fn helper() {}\n",
        "fn main() {}",
    );
    for (uri, text) in [
        (&entry, original),
        (&dependency, "pub fn answer(input: Bool) -> Int32 { if input { 42 } else { 0 } }\nfn private_answer() {}"),
    ] {
        client.send(json!({"method": "textDocument/didOpen", "params": {
            "textDocument": {"uri": uri.as_str(), "version": 1, "text": text}
        }}));
    }
    let mut complete = |version: u64, text: &str| {
        let (left, right) = text.split_once('¦').unwrap();
        let line = left.bytes().filter(|&c| c == b'\n').count();
        let character = left.rsplit('\n').next().unwrap().encode_utf16().count();
        let text = format!("{left}{right}");
        client.send(json!({"method": "textDocument/didChange", "params": {
            "textDocument": {"uri": entry.as_str(), "version": version},
            "contentChanges": [{"text": text}]
        }}));
        client.send(json!({"id": version, "method": "textDocument/completion", "params": {
            "textDocument": {"uri": entry.as_str()}, "position": {"line": line, "character": character},
        }}));
        let response = client.until(|msg| msg["id"] == version);
        assert!(response.get("error").is_none(), "{response}");
        response["result"]["items"].as_array().unwrap().clone()
    };
    let imports = "import lib/value as values\nimport lib/helpers.{calculate as convert}";
    let items = complete(2, &format!("{imports}\nfn main() {{ values.¦ }}"));
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0]["label"], "answer");
    assert_eq!(items[0]["detail"], "fn answer(input: Bool) -> Int32");
    let items = complete(
        3,
        &format!("{imports}\nfn main() {{ values.an¦swer(true) }}"),
    );
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(
        items[0]["textEdit"],
        json!({"newText": "answer", "range": {
            "start": {"line": 2, "character": 19}, "end": {"line": 2, "character": 25}
        }})
    );
    let items = complete(4, &format!(
        "{imports}\nfn helper() {{}}\nfn broken() {{ missing() }}\nfn run(argument: Int32) {{\n let local = argument\n {{ let gone = 2 }}\n {{ let local = true; println(¦\n }}\n}}\nfn main() {{}}"
    ));
    let item = |name: &str| items.iter().find(|item| item["label"] == name).unwrap();
    assert_eq!(item("local")["detail"], "Bool");
    assert_eq!(item("argument")["detail"], "Int32");
    assert_eq!(item("helper")["detail"], "fn helper()");
    assert_eq!(
        item("convert")["detail"],
        "fn calculate(value: Int32) -> Int32"
    );
    assert!(!items
        .iter()
        .any(|item| ["gone", "calculate", "private_answer"]
            .iter()
            .any(|name| item["label"] == *name)));
    assert!(!complete(
        5,
        &format!("{imports}\nfn main() {{ let values = 3; values.¦ }}")
    )
    .iter()
    .any(|item| item["label"] == "answer"));
    assert!(complete(
        6,
        &format!("{imports}\nfn main() {{ println(\"values.¦\") }}")
    )
    .is_empty());
    let items = complete(
        7,
        &format!("{imports}\nfn main() {{\n let 名称 = 42\n \"🙂\"; println(名¦"),
    );
    assert_eq!(items[0]["label"], "名称");
    assert_eq!(items[0]["detail"], "Int32");
    assert_eq!(
        items[0]["textEdit"]["range"],
        json!({
            "start": {"line": 4, "character": 15}, "end": {"line": 4, "character": 16}
        })
    );
}

#[test]
fn lsp_completes_visible_fields_and_inherent_methods_of_resolved_receivers() {
    let work = Workspace::new();
    work.write("joky.toml", "[project]\nname='app'\nentry='main.jk'");
    let model = concat!(
        "pub struct Item<T> { pub value: T, secret: Int32 }\n",
        "impl<T: Copy> Item<T> {\n",
        "    pub fn read(&self) -> T { self.value }\n",
        "    pub fn change(&mut self, value: T) { self.value = value }\n",
        "    pub fn take(self) -> T { self.value }\n",
        "    fn hidden(&self) -> Int32 { self.secret }\n",
        "    pub fn create(value: T) -> Self { Self { value: value, secret: 0 } }\n",
        "}\n",
        "pub struct Holder { pub item: Item<Int32> }\n",
        "pub fn load() -> Holder { Holder { item: Item.create(42) } }\n",
    );
    work.write("model.jk", model);
    work.write("main.jk", "import model\nfn main() {}");
    let uri = |relative: &str| Url::from_file_path(work.path().join(relative)).unwrap();
    let entry = uri("main.jk");
    let dependency = uri("model.jk");
    let mut client = Client::start(&work);
    client.send(json!({"id": 1, "method": "initialize", "params": {"rootUri": uri("").as_str()}}));
    client.until(|msg| msg["id"] == 1);
    let mut version = 1;
    let mut complete = |uri: &Url, marked: &str| {
        let (left, right) = marked.split_once('¦').unwrap();
        let line = left.bytes().filter(|&c| c == b'\n').count();
        let character = left.rsplit('\n').next().unwrap().encode_utf16().count();
        let source = format!("{left}{right}");
        client.send(json!({"method": "textDocument/didOpen", "params": {
            "textDocument": {"uri": uri.as_str(), "version": version, "text": source}
        }}));
        version += 1;
        client.send(json!({"id": version, "method": "textDocument/completion", "params": {
            "textDocument": {"uri": uri.as_str()}, "position": {"line": line, "character": character},
        }}));
        let response = client.until(|msg| msg["id"] == version);
        response["result"]["items"].as_array().unwrap().clone()
    };
    let cases = [
        (
            "fn main() { var item = model.load().item; item.¦ }",
            vec!["change", "read", "take", "value"],
        ),
        (
            "fn main() { var item = model.load().item; item.ch¦ange(42) }",
            vec!["change"],
        ),
        ("fn main() { model.load().¦ }", vec!["item"]),
        ("fn main() { model.load().item.re¦ad() }", vec!["read"]),
        (
            "fn inspect(item: &model.Item<Int32>) { item.¦ } fn main() {}",
            vec!["read", "value"],
        ),
        (
            "fn main() { let item = model.load().item; item.¦ }",
            vec!["read", "take", "value"],
        ),
        (
            "fn main() { let model = model.load(); model.¦ }",
            vec!["item"],
        ),
        ("fn main() { model.¦ }", vec!["load"]),
    ];
    for (source, expected) in cases {
        let items = complete(&entry, &format!("import model\n{source}"));
        let labels = items
            .iter()
            .map(|item| item["label"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(labels, expected, "{source}");
        for item in items {
            match item["label"].as_str().unwrap() {
                "value" => {
                    assert_eq!(item["kind"], 5);
                    assert_eq!(item["detail"], "Int32");
                }
                "change" => {
                    assert_eq!(item["kind"], 2);
                    assert_eq!(item["detail"], "fn change(value: Int32)");
                    if let Some((left, _)) = source.split_once("ch¦ange") {
                        let start = left.encode_utf16().count();
                        assert_eq!(
                            item["textEdit"],
                            json!({"newText": "change", "range": {
                                "start": {"line": 1, "character": start}, "end": {"line": 1, "character": start + 6}
                            }})
                        );
                    }
                }
                "read" => assert_eq!(item["detail"], "fn read() -> Int32"),
                _ => {}
            }
        }
    }
    let local = complete(
        &dependency,
        &format!("{model}fn inspect(item: &Item<Int32>) {{ item.¦ }}"),
    );
    assert!(local.iter().any(|item| item["label"] == "secret"));
    assert!(local.iter().any(|item| item["label"] == "hidden"));
}

#[test]
fn lsp_resolves_inferred_bindings_and_function_definitions_in_the_current_snapshot() {
    let work = Workspace::new();
    work.write("joky.toml", "[project]\nname='app'\nentry='main.jk'");
    let main = concat!(
        "\u{feff}import values as numbers\r\n",
        "import helpers.{identity as convert}\r\n",
        "fn local(input: Int32) -> Int32 {\r\n",
        "    let count = convert(input)\r\n",
        "    { let count = true; println(\"🙂{count}\") }\r\n",
        "    count\r\n",
        "}\r\n",
        "fn main() -> Int32 { local(numbers.answer()) }",
    );
    let helper = "pub fn identity<T>(value: T) -> T { value }";
    let dep = "pub fn answer() -> Int32 { 42 }";
    work.write("main.jk", main);
    work.write("helpers.jk", helper);
    work.write("values.jk", dep);
    let uri = |relative: &str| Url::from_file_path(work.path().join(relative)).unwrap();
    let entry = uri("main.jk");
    let helpers = uri("helpers.jk");
    let dependency = uri("values.jk");
    let mut client = Client::start(&work);
    client.send(json!({"id": 1, "method": "initialize", "params": {"rootUri": uri("").as_str()}}));
    let initialized = client.until(|message| message["id"] == 1);
    assert_eq!(initialized["result"]["capabilities"]["hoverProvider"], true);
    assert_eq!(
        initialized["result"]["capabilities"]["definitionProvider"],
        true
    );
    let open = |uri: &Url, text: &str| {
        json!({"method": "textDocument/didOpen", "params": {
            "textDocument": {"uri": uri.as_str(), "version": 1, "languageId": "joky", "text": text}
        }})
    };
    client.send(open(&entry, main));
    let mut id = 2;
    let mut request = |client: &mut Client, method: &str, uri: &Url, position: &Value| {
        id += 1;
        client.send(json!({"id": id, "method": method, "params": {
            "textDocument": {"uri": uri.as_str()}, "position": position,
        }}));
        let message = client.until(|message| message["id"] == id);
        assert!(message.get("error").is_none(), "{message}");
        message["result"].clone()
    };
    for (source, definition, destination, display) in [
        (
            symbol_range(main, 3, "input", false),
            symbol_range(main, 2, "input", false),
            &entry,
            "input: Int32",
        ),
        (
            symbol_range(main, 5, "count", false),
            symbol_range(main, 3, "count", false),
            &entry,
            "count: Int32",
        ),
        (
            symbol_range(main, 4, "count", true),
            symbol_range(main, 4, "count", false),
            &entry,
            "count: Bool",
        ),
        (
            symbol_range(main, 3, "convert", false),
            symbol_range(helper, 0, "identity", false),
            &helpers,
            "fn identity(value: Int32) -> Int32",
        ),
        (
            symbol_range(main, 7, "answer", false),
            symbol_range(dep, 0, "answer", false),
            &dependency,
            "fn answer() -> Int32",
        ),
        (
            symbol_range(main, 7, "local", false),
            symbol_range(main, 2, "local", false),
            &entry,
            "fn local(input: Int32) -> Int32",
        ),
    ] {
        assert_eq!(
            request(&mut client, "textDocument/hover", &entry, &source["start"]),
            json!({"contents": {"kind": "plaintext", "value": display}, "range": source})
        );
        assert_eq!(
            request(
                &mut client,
                "textDocument/definition",
                &entry,
                &source["start"]
            ),
            json!({"uri": destination.as_str(), "range": definition})
        );
    }
    let edited = format!("// Unsaved shift\n{dep}");
    client.send(open(&dependency, &edited));
    let call = symbol_range(main, 7, "answer", false);
    assert_eq!(
        request(
            &mut client,
            "textDocument/definition",
            &entry,
            &call["start"]
        ),
        json!({"uri": dependency.as_str(), "range": symbol_range(&edited, 1, "answer", false)})
    );
    let builtin = symbol_range(main, 4, "println", false);
    assert_eq!(
        request(&mut client, "textDocument/hover", &entry, &builtin["start"])["range"],
        builtin
    );
    assert!(request(
        &mut client,
        "textDocument/definition",
        &entry,
        &builtin["start"]
    )
    .is_null());
    assert!(request(
        &mut client,
        "textDocument/hover",
        &entry,
        &json!({"line": 1, "character": 0})
    )
    .is_null());
}

#[test]
fn lsp_checks_unsaved_path_dependencies_and_discards_old_document_versions() {
    let work = Workspace::new();
    work.write(
        "app/joky.toml",
        "[project]\nname='app'\nentry='main.jk'\n[dependencies]\nlib={path='../lib'}",
    );
    work.write("lib/joky.toml", "[package]\nname='lib'");
    let main = "import lib/value\nfn main() -> Int32 { value.answer() }";
    let original = "pub fn answer() -> Int32 { 42 }";
    work.write("app/main.jk", main);
    work.write("lib/value.jk", original);
    let uri = |relative: &str| Url::from_file_path(work.path().join(relative)).unwrap();
    let entry = uri("app/main.jk");
    let dependency = uri("lib/value.jk");
    let mut client = Client::start(&work);
    client.send(json!({"id": 1, "method": "initialize", "params": {
        "rootUri": uri("").as_str(), "capabilities": {"textDocument": {"publishDiagnostics": {"versionSupport": true}}},
        "initializationOptions": {"jokySnapshotVersions": true}
    }}));
    let initialized = client.until(|message| message["id"] == 1);
    assert_eq!(
        initialized["result"]["capabilities"]["positionEncoding"],
        "utf-16"
    );
    assert_eq!(
        initialized["result"]["capabilities"]["textDocumentSync"]["change"],
        1
    );
    client.send(json!({"method": "initialized", "params": {}}));
    for (uri, text, revision) in [(&entry, main, 1), (&dependency, original, 2)] {
        client.send(json!({"method": "textDocument/didOpen", "params": {
            "jokySnapshotRevision": revision,
            "textDocument": {"uri": uri.as_str(), "languageId": "joky", "version": 1, "text": text}
        }}));
    }
    client.report(&dependency, 1, 2, 0);
    let call = symbol_range(main, 1, "answer", false);
    client.send(
        json!({"id": 3, "method": "textDocument/definition", "params": {
            "textDocument": {"uri": entry.as_str()}, "position": call["start"]
        }}),
    );
    assert_eq!(
        client.until(|message| message["id"] == 3)["result"],
        json!({"uri": dependency.as_str(), "range": symbol_range(original, 0, "answer", false)})
    );
    let invalid = "\u{feff}// header\r\npub fn answer() -> Int32 { let text = \"🙂\" $ }";
    let change = |text: &str, version: u64, revision: u64| {
        json!({"method": "textDocument/didChange", "params": {
            "jokySnapshotRevision": revision, "textDocument": {"uri": dependency.as_str(), "version": version},
            "contentChanges": [{"text": text}]
        }})
    };
    client.send(change(invalid, 2, 3));
    let report = client.report(&dependency, 2, 3, 1);
    let error = &report["params"]["diagnostics"][0];
    assert_eq!(error["code"], "syntax_error");
    let column = invalid
        .split("\r\n")
        .nth(1)
        .unwrap()
        .split('$')
        .next()
        .unwrap()
        .encode_utf16()
        .count();
    assert_eq!(
        error["range"],
        json!({"start": {"line": 1, "character": column}, "end": {"line": 1, "character": column + 1}})
    );
    client.send(change(original, 4, 4));
    client.send(change(invalid, 3, 5));
    client.report(&dependency, 4, 4, 0);
    client.send(change("pub fn answer() -> Bool { true }", 5, 6));
    let report = client.report(&entry, 1, 6, 1);
    assert_eq!(report["params"]["diagnostics"][0]["code"], "type_mismatch");
    assert_eq!(
        report["params"]["jokySnapshotVersions"][dependency.as_str()],
        5
    );
    assert_eq!(
        std::fs::read_to_string(work.path().join("lib/value.jk")).unwrap(),
        original
    );

    client.send(json!({"method": "textDocument/didClose", "params": {
        "jokySnapshotRevision": 7, "textDocument": {"uri": dependency.as_str()}
    }}));
    let result = client.report(&entry, 1, 7, 0);
    assert!(result["params"]["jokySnapshotVersions"]
        .get(dependency.as_str())
        .is_none());
    client.send(json!({"id": 2, "method": "shutdown", "params": null}));
    assert!(client.until(|message| message["id"] == 2)["result"].is_null());
    client.send(json!({"method": "exit"}));
    assert!(client.child.wait().unwrap().success());
}
