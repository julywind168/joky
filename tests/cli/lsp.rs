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
