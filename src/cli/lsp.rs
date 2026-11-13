//! Full-document synchronization and static diagnostics over LSP stdio.
mod wire;

use super::check;
use joky::{Error, Span};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, BufRead, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc,
    },
    thread,
    time::Duration,
};
use url::Url;

const DEBOUNCE: Duration = Duration::from_millis(150);

#[derive(Clone)]
struct Document {
    path: PathBuf,
    version: i64,
    text: Arc<str>,
}

struct Job {
    revision: u64,
    documents: BTreeMap<String, Document>,
    roots: Vec<PathBuf>,
}

struct Report {
    version: Option<i64>,
    diagnostic: Value,
}

enum Event {
    Input(io::Result<Option<serde_json::Result<Value>>>),
    Checked(u64, BTreeMap<String, Report>, Vec<String>),
}

#[derive(Default)]
struct Session {
    initialized: bool,
    shutdown: bool,
    documents: BTreeMap<String, Document>,
    roots: Vec<PathBuf>,
    published: BTreeSet<String>,
    revision: u64,
    pending: bool,
    snapshot_versions: bool,
    diagnostic_versions: bool,
    related_information: bool,
    client_revision: Option<u64>,
}

pub(super) fn execute() -> u8 {
    let (events, receiver) = mpsc::channel();
    let reader = events.clone();
    thread::spawn(move || read_messages(io::stdin().lock(), reader));
    let (jobs, work) = mpsc::sync_channel::<Job>(1);
    let revision = Arc::new(AtomicU64::new(0));
    let current = revision.clone();
    thread::spawn(move || {
        let checker = joky::Checker::new();
        while let Ok(mut job) = work.recv() {
            while let Ok(newer) = work.try_recv() {
                job = newer;
            }
            if job.revision != current.load(Ordering::Relaxed) {
                continue;
            }
            let (reports, messages) = analyze(&job, &checker);
            if events
                .send(Event::Checked(job.revision, reports, messages))
                .is_err()
            {
                break;
            }
        }
    });
    match serve(receiver, jobs, &revision, &mut io::stdout().lock()) {
        Ok(status) => status,
        Err(error) => {
            eprintln!("error: LSP transport: {error}");
            1
        }
    }
}

fn read_messages(mut input: impl BufRead, events: mpsc::Sender<Event>) {
    loop {
        let message = wire::read(&mut input);
        let done = !matches!(message, Ok(Some(_)));
        if events.send(Event::Input(message)).is_err() || done {
            break;
        }
    }
}

fn serve(
    events: mpsc::Receiver<Event>,
    jobs: mpsc::SyncSender<Job>,
    revision: &AtomicU64,
    out: &mut impl Write,
) -> io::Result<u8> {
    let mut session = Session::default();
    loop {
        let event = if session.pending {
            match events.recv_timeout(DEBOUNCE) {
                Ok(event) => event,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    let job = Job {
                        revision: session.revision,
                        documents: session.documents.clone(),
                        roots: session.roots.clone(),
                    };
                    match jobs.try_send(job) {
                        Ok(()) => session.pending = false,
                        Err(mpsc::TrySendError::Full(_)) => {}
                        Err(mpsc::TrySendError::Disconnected(_)) => {
                            return Err(io::Error::other("LSP check worker stopped"));
                        }
                    }
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(1),
            }
        } else {
            match events.recv() {
                Ok(event) => event,
                Err(_) => return Ok(u8::from(!session.shutdown)),
            }
        };
        match event {
            Event::Input(Ok(Some(Ok(message)))) => {
                if let Some(status) = session.message(message, out)? {
                    return Ok(status);
                }
                revision.store(session.revision, Ordering::Relaxed);
            }
            Event::Input(Ok(Some(Err(_)))) => failure(out, &Value::Null, -32700, "Parse error")?,
            Event::Input(Ok(None)) => return Ok(u8::from(!session.shutdown)),
            Event::Input(Err(error)) => return Err(error),
            Event::Checked(revision, reports, messages) => {
                if revision == session.revision && !session.shutdown {
                    session.publish(reports, messages, out)?;
                }
            }
        }
    }
}

impl Session {
    fn message(&mut self, message: Value, out: &mut impl Write) -> io::Result<Option<u8>> {
        let id = message.get("id");
        let Some(method) = message["method"]
            .as_str()
            .filter(|_| message["jsonrpc"] == "2.0")
        else {
            failure(out, id.unwrap_or(&Value::Null), -32600, "Invalid request")?;
            return Ok(None);
        };
        let params = &message["params"];
        if method == "exit" {
            return Ok(Some(u8::from(!self.shutdown)));
        }
        match (method, id) {
            ("initialize", Some(id)) if !self.initialized => {
                self.initialized = true;
                self.snapshot_versions =
                    params["initializationOptions"]["jokySnapshotVersions"] == true;
                let capabilities = &params["capabilities"]["textDocument"]["publishDiagnostics"];
                self.diagnostic_versions = capabilities["versionSupport"] == true;
                self.related_information = capabilities["relatedInformation"] == true;
                self.roots = params["workspaceFolders"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|folder| folder["uri"].as_str().and_then(file_path))
                    .collect();
                if self.roots.is_empty() {
                    self.roots
                        .extend(params["rootUri"].as_str().and_then(file_path));
                }
                response(
                    out,
                    id,
                    json!({
                        "capabilities": {
                            "positionEncoding": "utf-16",
                            "textDocumentSync": {"openClose": true, "change": 1, "save": true}
                        },
                        "serverInfo": {"name": "Joky", "version": env!("CARGO_PKG_VERSION")}
                    }),
                )?;
            }
            (_, Some(id)) if !self.initialized => {
                failure(out, id, -32002, "Server not initialized")?
            }
            (_, Some(id)) if self.shutdown => failure(out, id, -32600, "Server is shutting down")?,
            ("shutdown", Some(id)) => {
                self.shutdown = true;
                self.pending = false;
                response(out, id, Value::Null)?;
            }
            (_, Some(id)) => failure(out, id, -32601, "Method not found")?,
            (_, None) if self.initialized && !self.shutdown && self.update(method, params) => {
                self.client_revision = params["jokySnapshotRevision"].as_u64();
                self.revision += 1;
                self.pending = true;
                for uri in std::mem::take(&mut self.published) {
                    self.diagnostics(out, &uri, None)?;
                }
            }
            _ => {}
        }
        Ok(None)
    }

    fn update(&mut self, method: &str, params: &Value) -> bool {
        if method == "workspace/didChangeWatchedFiles" {
            return true;
        }
        let document = &params["textDocument"];
        let Some(uri) = document["uri"].as_str() else {
            return false;
        };
        match method {
            "textDocument/didOpen" => {
                let Some(path) =
                    file_path(uri).filter(|path| path.extension().is_some_and(|ext| ext == "jk"))
                else {
                    return false;
                };
                let (Some(version), Some(text)) =
                    (document["version"].as_i64(), document["text"].as_str())
                else {
                    return false;
                };
                self.documents.insert(
                    uri.to_owned(),
                    Document {
                        path,
                        version,
                        text: text.into(),
                    },
                );
                true
            }
            "textDocument/didChange" => {
                let Some(open) = self.documents.get_mut(uri) else {
                    return false;
                };
                let Some(version) = document["version"]
                    .as_i64()
                    .filter(|version| *version > open.version)
                else {
                    return false;
                };
                let Some(change) = params["contentChanges"]
                    .as_array()
                    .and_then(|changes| changes.last())
                else {
                    return false;
                };
                let Some(text) = change["text"]
                    .as_str()
                    .filter(|_| change.get("range").is_none())
                else {
                    return false;
                };
                open.version = version;
                open.text = text.into();
                true
            }
            "textDocument/didClose" => self.documents.remove(uri).is_some(),
            "textDocument/didSave" => file_path(uri).is_some_and(|path| {
                path.extension().is_some_and(|ext| ext == "jk")
                    || path.file_name().is_some_and(|name| name == "joky.toml")
            }),
            _ => false,
        }
    }

    fn diagnostics(
        &self,
        out: &mut impl Write,
        uri: &str,
        report: Option<Report>,
    ) -> io::Result<()> {
        let version = self.documents.get(uri).map(|document| document.version);
        let diagnostics = report
            .into_iter()
            .map(|report| {
                let mut diagnostic = report.diagnostic;
                if !self.related_information {
                    diagnostic
                        .as_object_mut()
                        .expect("diagnostic object")
                        .remove("relatedInformation");
                }
                diagnostic
            })
            .collect::<Vec<_>>();
        let mut params = json!({"uri": uri, "diagnostics": diagnostics});
        if let Some(version) = version.filter(|_| self.diagnostic_versions) {
            params["version"] = json!(version);
        }
        if self.snapshot_versions {
            params["jokySnapshotRevision"] = json!(self.client_revision);
            params["jokySnapshotVersions"] = json!(self
                .documents
                .iter()
                .map(|(uri, document)| (uri, document.version))
                .collect::<BTreeMap<_, _>>());
        }
        wire::write(
            out,
            &json!({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": params}),
        )
    }

    fn publish(
        &mut self,
        reports: BTreeMap<String, Report>,
        messages: Vec<String>,
        out: &mut impl Write,
    ) -> io::Result<()> {
        for uri in self.documents.keys() {
            if !reports.contains_key(uri) {
                self.diagnostics(out, uri, None)?;
            }
        }
        for (uri, report) in reports {
            if self.documents.get(&uri).map(|document| document.version) == report.version {
                self.diagnostics(out, &uri, Some(report))?;
                self.published.insert(uri);
            }
        }
        for message in messages {
            wire::write(
                out,
                &json!({"jsonrpc": "2.0", "method": "window/logMessage", "params": {"type": 2, "message": message}}),
            )?;
        }
        Ok(())
    }
}

fn response(out: &mut impl Write, id: &Value, result: Value) -> io::Result<()> {
    wire::write(out, &json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

fn failure(out: &mut impl Write, id: &Value, code: i32, message: &str) -> io::Result<()> {
    wire::write(
        out,
        &json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}),
    )
}

fn file_path(uri: &str) -> Option<PathBuf> {
    Url::parse(uri).ok()?.to_file_path().ok()
}

fn target(file: &Path, roots: &[PathBuf]) -> PathBuf {
    let boundary = roots
        .iter()
        .filter(|root| file.starts_with(root))
        .max_by_key(|root| root.components().count());
    for directory in file.parent().into_iter().flat_map(Path::ancestors) {
        let manifest = directory.join("joky.toml");
        if manifest.is_file() {
            return manifest;
        }
        if boundary.is_some_and(|root| directory == root) {
            break;
        }
    }
    file.to_owned()
}

fn analyze(job: &Job, checker: &joky::Checker) -> (BTreeMap<String, Report>, Vec<String>) {
    let mut files = BTreeMap::new();
    let mut overrides = BTreeMap::new();
    let mut targets = BTreeMap::<_, Vec<_>>::new();
    let mut reports = BTreeMap::new();
    let mut messages = Vec::new();
    for (uri, document) in &job.documents {
        let Ok(path) = document.path.canonicalize() else {
            check_syntax(&mut reports, uri, document);
            continue;
        };
        files.insert(path.clone(), (uri.as_str(), document.version));
        overrides.insert(path, document.text.clone());
        targets
            .entry(target(&document.path, &job.roots))
            .or_default()
            .push((uri, document));
    }
    for (path, documents) in targets {
        // Check the project root once so imported files are analyzed in their own package context.
        if let Err(error) = check::check_with_overrides(&path, checker, &overrides) {
            if let Some((uri, version, diagnostic)) = diagnostic(&error, &files, &path) {
                reports.entry(uri).or_insert(Report {
                    version,
                    diagnostic,
                });
            } else {
                messages.push(format!(
                    "{}: [{}] {}",
                    path.display(),
                    error.code(),
                    error.message()
                ));
            }
        }
        // An application may have open files outside its entry's import graph.
        // Catch syntax errors there without resolving them with a different project root.
        if path.file_name().is_some_and(|name| name == "joky.toml") {
            for (uri, document) in documents {
                check_syntax(&mut reports, uri, document);
            }
        }
    }
    (reports, messages)
}

fn check_syntax(reports: &mut BTreeMap<String, Report>, uri: &str, document: &Document) {
    if reports.contains_key(uri) {
        return;
    }
    if let Err(error) = joky::parser::parse(&document.text) {
        if error.span.slice(&document.text).is_none() {
            return;
        }
        let range = json!({"start": position(&document.text, error.span.start()), "end": position(&document.text, error.span.end())});
        reports.insert(uri.to_owned(), Report {
            version: Some(document.version),
            diagnostic: json!({"range": range, "severity": 1, "source": "Joky", "code": error.code.as_str(), "message": error.message}),
        });
    }
}

fn diagnostic(
    error: &Error,
    files: &BTreeMap<PathBuf, (&str, i64)>,
    target: &Path,
) -> Option<(String, Option<i64>, Value)> {
    let display_root = target.parent()?;
    let root = display_root.canonicalize().ok();
    let location = |span: Span| {
        let file = error.sources().get(span.source_id())?;
        let path = file.path()?;
        let (uri, version) = match files.get(path) {
            Some((uri, version)) => ((*uri).to_owned(), Some(*version)),
            None => {
                let display = root
                    .as_ref()
                    .and_then(|root| path.strip_prefix(root).ok())
                    .map(|relative| display_root.join(relative));
                (
                    Url::from_file_path(display.as_deref().unwrap_or(path))
                        .ok()?
                        .into(),
                    None,
                )
            }
        };
        span.slice(file.text())?;
        let range = json!({"start": position(file.text(), span.start()), "end": position(file.text(), span.end())});
        Some((uri, version, range))
    };
    let (uri, version, range) = location(error.span()?)?;
    let related: Vec<_> = error
        .labels()
        .iter()
        .filter_map(|label| {
            let (uri, _, range) = location(label.span)?;
            Some(json!({"location": {"uri": uri, "range": range}, "message": label.message}))
        })
        .collect();
    let message = std::iter::once(error.message())
        .chain(error.help().iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    Some((
        uri,
        version,
        json!({
            "range": range, "severity": 1, "code": error.code().as_str(), "source": "Joky",
            "message": message, "relatedInformation": related,
        }),
    ))
}

fn position(text: &str, offset: usize) -> Value {
    let mut line = 0;
    let mut character = 0;
    let mut cr = false;
    let prefix = &text[..offset];
    for ch in prefix.strip_prefix('\u{feff}').unwrap_or(prefix).chars() {
        match ch {
            '\r' => {
                line += 1;
                character = 0;
            }
            '\n' if !cr => {
                line += 1;
                character = 0;
            }
            '\n' => {}
            _ => character += ch.len_utf16(),
        }
        cr = ch == '\r';
    }
    json!({"line": line, "character": character})
}
