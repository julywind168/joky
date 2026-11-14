//! Full-document synchronization and language features over LSP stdio.
mod completion;
#[cfg(test)]
mod tests;
mod wire;

use super::check;
use super::project::Project;
use joky::{EditorParseCache, EditorSources, Error, ErrorPhase, SemanticIndex, SourceFile, Span};
use serde_json::{json, Value};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, VecDeque},
    io::{self, BufRead, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc,
    },
    thread,
    time::{Duration, Instant},
};
use url::Url;

const DEBOUNCE: Duration = Duration::from_millis(150);
const COMPLETION_DISPATCH: Duration = Duration::from_millis(5);

#[derive(Clone)]
struct Document {
    path: PathBuf,
    version: i64,
    text: Arc<str>,
}

#[derive(Default)]
struct Snapshot {
    revision: u64,
    documents: BTreeMap<String, Document>,
    files: BTreeMap<PathBuf, String>,
    overrides: BTreeMap<PathBuf, Arc<str>>,
    paths: BTreeMap<String, PathBuf>,
    targets: BTreeMap<PathBuf, Vec<String>>,
    document_targets: BTreeMap<String, PathBuf>,
    client_versions: Option<Value>,
}

enum Work {
    Check(Arc<Snapshot>, PathBuf),
    Complete(Arc<Snapshot>, CompletionRequest),
}

#[derive(Clone, PartialEq)]
struct Report {
    version: Option<i64>,
    diagnostic: Value,
}

enum Event {
    Input(io::Result<Option<serde_json::Result<Value>>>),
    Checked(u64, PathBuf, Option<Analysis>),
    Completed(u64, Value),
}

#[derive(Default)]
struct Analysis {
    reports: BTreeMap<String, Report>,
    messages: Vec<String>,
    indexes: Vec<SemanticIndex>,
    inputs: BTreeSet<PathBuf>,
    library_root: Option<PathBuf>,
}

enum QueryKind {
    Hover,
    Definition,
    Completion,
}

struct Query {
    id: Value,
    kind: QueryKind,
    uri: String,
    path: PathBuf,
    target: PathBuf,
    offset: usize,
}

struct CompletionRequest {
    ticket: u64,
    query: Query,
    cancelled: Arc<AtomicBool>,
}

#[derive(Default)]
struct Session {
    initialized: bool,
    shutdown: bool,
    documents: BTreeMap<String, Document>,
    roots: Vec<PathBuf>,
    published: BTreeMap<String, Report>,
    snapshot: Arc<Snapshot>,
    check_after: Option<Instant>,
    completion_after: Option<Instant>,
    snapshot_versions: bool,
    diagnostic_versions: bool,
    related_information: bool,
    analyses: BTreeMap<PathBuf, Analysis>,
    queries: Vec<Query>,
    completions: VecDeque<CompletionRequest>,
    completing: BTreeMap<u64, (Value, Arc<AtomicBool>)>,
    next_completion: u64,
}

pub(super) fn execute() -> u8 {
    let (events, receiver) = mpsc::channel();
    let reader = events.clone();
    thread::spawn(move || read_messages(io::stdin().lock(), reader));
    let (jobs, work) = mpsc::channel::<Work>();
    let revision = Arc::new(AtomicU64::new(0));
    let current = revision.clone();
    thread::spawn(move || {
        let checker = joky::Checker::new();
        let cache = EditorParseCache::default();
        while let Ok(work) = work.recv() {
            let event = match work {
                Work::Check(snapshot, path) => {
                    let stale = || snapshot.revision != current.load(Ordering::Relaxed);
                    let result = analyze(&snapshot, &path, &checker, &cache, stale);
                    Event::Checked(snapshot.revision, path, result)
                }
                Work::Complete(snapshot, request) => {
                    let stale = || {
                        snapshot.revision != current.load(Ordering::Relaxed)
                            || request.cancelled.load(Ordering::Relaxed)
                    };
                    let result =
                        completion::complete(&snapshot, &request.query, &checker, &cache, stale);
                    Event::Completed(request.ticket, result)
                }
            };
            cache.trim();
            if events.send(event).is_err() {
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
    jobs: mpsc::Sender<Work>,
    revision: &AtomicU64,
    out: &mut impl Write,
) -> io::Result<u8> {
    let mut session = Session::default();
    let mut busy = false;
    loop {
        let event = if let Some(deadline) = (!busy).then(|| session.deadline()).flatten() {
            match events.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(event) => event,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    let work = session.next_work().expect("scheduled work");
                    jobs.send(work)
                        .map_err(|_| io::Error::other("LSP check worker stopped"))?;
                    busy = true;
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
                revision.store(session.snapshot.revision, Ordering::Relaxed);
            }
            Event::Input(Ok(Some(Err(_)))) => failure(out, &Value::Null, -32700, "Parse error")?,
            Event::Input(Ok(None)) => return Ok(u8::from(!session.shutdown)),
            Event::Input(Err(error)) => return Err(error),
            Event::Checked(revision, path, analysis) => {
                busy = false;
                if revision == session.snapshot.revision && !session.shutdown {
                    if let Some(analysis) = analysis {
                        session.accept(path, analysis, out)?;
                    }
                }
            }
            Event::Completed(ticket, result) => {
                busy = false;
                if let Some((id, _)) = session.completing.remove(&ticket) {
                    response(out, &id, result)?;
                }
            }
        }
    }
}

impl Session {
    fn deadline(&self) -> Option<Instant> {
        if self.shutdown {
            return None;
        }
        if !self.completions.is_empty() {
            self.completion_after
        } else if !self.queries.is_empty() {
            Some(Instant::now())
        } else {
            self.check_after
        }
    }

    fn next_work(&mut self) -> Option<Work> {
        let snapshot = self.snapshot.clone();
        if let Some(request) = self.completions.pop_front() {
            if self.completions.is_empty() {
                self.completion_after = None;
            }
            return Some(Work::Complete(snapshot, request));
        }
        let target = self
            .queries
            .iter()
            .find(|query| !self.analyses.contains_key(&query.target))
            .map(|query| &query.target)
            .or_else(|| {
                snapshot
                    .targets
                    .keys()
                    .find(|path| !self.analyses.contains_key(*path))
            })?
            .clone();
        Some(Work::Check(snapshot, target))
    }

    fn accept(
        &mut self,
        path: PathBuf,
        analysis: Analysis,
        out: &mut impl Write,
    ) -> io::Result<()> {
        for message in &analysis.messages {
            wire::write(
                out,
                &json!({"jsonrpc": "2.0", "method": "window/logMessage", "params": {"type": 2, "message": message}}),
            )?;
        }
        self.analyses.insert(path, analysis);
        let complete = self.analyses.len() == self.snapshot.targets.len();
        for query in std::mem::take(&mut self.queries) {
            if complete || self.analyses.contains_key(&query.target) {
                let result = self.lookup(&query);
                if complete || !result.is_null() {
                    response(out, &query.id, result)?;
                    continue;
                }
            }
            self.queries.push(query);
        }
        if complete {
            self.check_after = None;
            self.publish(out)?;
        }
        Ok(())
    }

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
                            "experimental": {"jokyScopedDiagnostics": true},
                            "hoverProvider": true,
                            "definitionProvider": true,
                            "completionProvider": {"triggerCharacters": ["."]},
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
                self.check_after = None;
                self.clear_queries(out)?;
                response(out, id, Value::Null)?;
            }
            (
                "textDocument/hover" | "textDocument/definition" | "textDocument/completion",
                Some(id),
            ) => {
                let kind = match method {
                    "textDocument/hover" => QueryKind::Hover,
                    "textDocument/definition" => QueryKind::Definition,
                    _ => QueryKind::Completion,
                };
                self.query(id, kind, params, out)?;
            }
            (_, Some(id)) => failure(out, id, -32601, "Method not found")?,
            ("$/cancelRequest", None) => {
                if let Some(index) = self
                    .queries
                    .iter()
                    .position(|query| query.id == params["id"])
                {
                    let query = self.queries.remove(index);
                    failure(out, &query.id, -32800, "Request cancelled")?;
                }
                let ticket = self
                    .completing
                    .iter()
                    .find_map(|(ticket, (id, _))| (*id == params["id"]).then_some(*ticket));
                if let Some(ticket) = ticket {
                    let (id, cancelled) = self.completing.remove(&ticket).unwrap();
                    cancelled.store(true, Ordering::Relaxed);
                    self.completions.retain(|request| request.ticket != ticket);
                    failure(out, &id, -32800, "Request cancelled")?;
                }
            }
            (_, None) if self.initialized && !self.shutdown => {
                if let Some(changed) = self.update(method, params) {
                    self.refresh(changed, params["jokySnapshotRevision"].as_u64(), out)?;
                }
            }
            _ => {}
        }
        Ok(None)
    }

    fn clear_queries(&mut self, out: &mut impl Write) -> io::Result<()> {
        for query in self.queries.drain(..) {
            response(out, &query.id, Value::Null)?;
        }
        self.completions.clear();
        self.completion_after = None;
        for (id, cancelled) in std::mem::take(&mut self.completing).into_values() {
            cancelled.store(true, Ordering::Relaxed);
            response(out, &id, Value::Null)?;
        }
        Ok(())
    }

    fn query(
        &mut self,
        id: &Value,
        kind: QueryKind,
        params: &Value,
        out: &mut impl Write,
    ) -> io::Result<()> {
        let Some(uri) = params["textDocument"]["uri"].as_str() else {
            return failure(out, id, -32602, "Expected a document URI");
        };
        let position = &params["position"];
        let (Some(line), Some(column)) =
            (position["line"].as_u64(), position["character"].as_u64())
        else {
            return failure(out, id, -32602, "Expected a document position");
        };
        let Some(document) = self.documents.get(uri) else {
            return response(out, id, Value::Null);
        };
        let Some(offset) = offset(&document.text, line, column) else {
            return response(out, id, Value::Null);
        };
        let Some((path, target)) = self
            .snapshot
            .paths
            .get(uri)
            .zip(self.snapshot.document_targets.get(uri))
        else {
            return response(out, id, Value::Null);
        };
        let query = Query {
            id: id.clone(),
            kind,
            uri: uri.to_owned(),
            path: path.clone(),
            target: target.clone(),
            offset,
        };
        if matches!(query.kind, QueryKind::Completion) {
            let ticket = self.next_completion;
            self.next_completion += 1;
            let cancelled = Arc::new(AtomicBool::new(false));
            if self.completions.is_empty() {
                self.completion_after = Some(Instant::now() + COMPLETION_DISPATCH);
            }
            self.completing
                .insert(ticket, (id.clone(), cancelled.clone()));
            self.completions.push_back(CompletionRequest {
                ticket,
                query,
                cancelled,
            });
            Ok(())
        } else {
            if self.analyses.contains_key(&query.target) {
                let result = self.lookup(&query);
                if !result.is_null() || self.check_after.is_none() {
                    return response(out, id, result);
                }
            }
            self.queries.push(query);
            Ok(())
        }
    }

    fn lookup(&self, query: &Query) -> Value {
        let own = self.analyses.get_key_value(&query.target);
        let found = own
            .into_iter()
            .chain(
                self.analyses
                    .iter()
                    .filter(|(target, _)| **target != query.target),
            )
            .flat_map(|(target, analysis)| {
                analysis.indexes.iter().map(move |index| (target, index))
            })
            .find_map(|(target, index)| {
                let file = index
                    .sources()
                    .files()
                    .find(|file| file.path() == Some(query.path.as_path()))?;
                let symbol = index.symbol_at(file.id(), query.offset)?;
                Some((target, index, file, symbol))
            });
        let Some((target, index, file, symbol)) = found else {
            return Value::Null;
        };
        match query.kind {
            QueryKind::Hover => json!({
                "contents": {"kind": "plaintext", "value": symbol.display},
                "range": source_range(file, symbol.occurrence),
            }),
            QueryKind::Definition => {
                let Some(definition) = symbol.definition else {
                    return Value::Null;
                };
                let Some(file) = index.sources().get(definition.source_id()) else {
                    return Value::Null;
                };
                let Some(path) = file.path() else {
                    return Value::Null;
                };
                let uri = self
                    .snapshot
                    .files
                    .get(path)
                    .cloned()
                    .or_else(|| display_uri(path, target));
                uri.map_or(Value::Null, |uri| {
                    json!({
                        "uri": uri,
                    "range": source_range(file, definition),
                    })
                })
            }
            QueryKind::Completion => unreachable!("completion runs on the checker worker"),
        }
    }

    fn update(&mut self, method: &str, params: &Value) -> Option<Vec<PathBuf>> {
        if method == "workspace/didChangeWatchedFiles" {
            let paths = params["changes"]
                .as_array()?
                .iter()
                .filter_map(|change| file_path(change["uri"].as_str()?))
                .collect::<Vec<_>>();
            return (!paths.is_empty()).then_some(paths);
        }
        let document = &params["textDocument"];
        let uri = document["uri"].as_str()?;
        let path = match method {
            "textDocument/didOpen" => {
                let path = file_path(uri)
                    .filter(|path| path.extension().is_some_and(|ext| ext == "jk"))?;
                let (Some(version), Some(text)) =
                    (document["version"].as_i64(), document["text"].as_str())
                else {
                    return None;
                };
                self.documents.insert(
                    uri.to_owned(),
                    Document {
                        path: path.clone(),
                        version,
                        text: text.into(),
                    },
                );
                path
            }
            "textDocument/didChange" => {
                let open = self.documents.get_mut(uri)?;
                let version = document["version"]
                    .as_i64()
                    .filter(|version| *version > open.version)?;
                let change = params["contentChanges"]
                    .as_array()
                    .and_then(|changes| changes.last())?;
                let text = change["text"]
                    .as_str()
                    .filter(|_| change.get("range").is_none())?;
                open.version = version;
                open.text = text.into();
                open.path.clone()
            }
            "textDocument/didClose" => self.documents.remove(uri)?.path,
            "textDocument/didSave" => file_path(uri).filter(|path| {
                path.extension().is_some_and(|ext| ext == "jk")
                    || path.file_name().is_some_and(|name| name == "joky.toml")
            })?,
            _ => return None,
        };
        Some(vec![path])
    }

    fn refresh(
        &mut self,
        changed: Vec<PathBuf>,
        client_revision: Option<u64>,
        out: &mut impl Write,
    ) -> io::Result<()> {
        let mut snapshot = Snapshot::new(self.snapshot.revision + 1, &self.documents, &self.roots);
        if self.snapshot_versions {
            snapshot.client_versions = Some(json!({
                "jokySnapshotRevision": client_revision,
                "jokySnapshotVersions": self.documents.iter()
                    .map(|(uri, document)| (uri, document.version))
                    .collect::<BTreeMap<_, _>>(),
            }));
        }
        let mut changed: BTreeSet<_> = changed
            .into_iter()
            .flat_map(|path| {
                let canonical = path
                    .canonicalize()
                    .ok()
                    .or_else(|| Some(path.parent()?.canonicalize().ok()?.join(path.file_name()?)));
                std::iter::once(path).chain(canonical)
            })
            .collect();
        for (uri, previous) in &self.snapshot.paths {
            if snapshot.paths.get(uri) != Some(previous) {
                changed.insert(previous.clone());
            }
        }
        self.analyses.retain(|path, analysis| {
            let Some(documents) = snapshot.targets.get(path) else {
                return false;
            };
            snapshot.targets.get(path) == self.snapshot.targets.get(path)
                && analysis.inputs.is_disjoint(&changed)
                && !changed.iter().any(|change| {
                    analysis.library_root.as_ref().is_some_and(|root| {
                        change.starts_with(root)
                            && change.extension().is_some_and(|ext| ext == "jk")
                    })
                })
                && !documents.iter().any(|uri| {
                    changed.contains(&snapshot.documents[uri].path)
                        || snapshot.paths.get(uri) != self.snapshot.paths.get(uri)
                        || snapshot
                            .paths
                            .get(uri)
                            .is_some_and(|path| changed.contains(path))
                })
        });
        self.snapshot = Arc::new(snapshot);
        self.clear_queries(out)?;
        let reports = self.reports();
        // Retract reports whose inputs changed before the next check finishes.
        let mut published = BTreeMap::new();
        for (uri, previous) in &self.published {
            if reports
                .get(uri.as_str())
                .is_some_and(|report| **report == *previous)
            {
                published.insert(uri.clone(), previous.clone());
            } else {
                self.diagnostics(out, uri, None)?;
            }
        }
        self.published = published;
        if self.analyses.len() == self.snapshot.targets.len() {
            self.check_after = None;
            self.publish(out)
        } else {
            self.check_after = Some(Instant::now() + DEBOUNCE);
            Ok(())
        }
    }

    fn diagnostics(
        &self,
        out: &mut impl Write,
        uri: &str,
        report: Option<&Report>,
    ) -> io::Result<()> {
        let version = self.documents.get(uri).map(|document| document.version);
        let diagnostics = report
            .into_iter()
            .map(|report| {
                let mut diagnostic = report.diagnostic.clone();
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
        if let Some(versions) = &self.snapshot.client_versions {
            let fields = params.as_object_mut().expect("diagnostic params");
            fields.extend(versions.as_object().expect("snapshot versions").clone());
        }
        wire::write(
            out,
            &json!({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": params}),
        )
    }

    fn reports(&self) -> BTreeMap<&str, &Report> {
        let mut reports = BTreeMap::new();
        for analysis in self.analyses.values() {
            for (uri, report) in &analysis.reports {
                if self.documents.get(uri).map(|document| document.version) == report.version {
                    reports.entry(uri.as_str()).or_insert(report);
                }
            }
        }
        reports
    }

    fn publish(&mut self, out: &mut impl Write) -> io::Result<()> {
        let reports = self.reports();
        for uri in self.documents.keys() {
            if !reports.contains_key(uri.as_str()) {
                self.diagnostics(out, uri, None)?;
            }
        }
        for (uri, report) in &reports {
            self.diagnostics(out, uri, Some(report))?;
        }
        self.published = reports
            .into_iter()
            .map(|(uri, report)| (uri.to_owned(), report.clone()))
            .collect();
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

impl Snapshot {
    fn new(revision: u64, documents: &BTreeMap<String, Document>, roots: &[PathBuf]) -> Self {
        let mut snapshot = Self {
            revision,
            documents: documents.clone(),
            ..Self::default()
        };
        for (uri, document) in documents {
            let project = target(&document.path, roots);
            snapshot
                .targets
                .entry(project.clone())
                .or_default()
                .push(uri.clone());
            snapshot.document_targets.insert(uri.clone(), project);
            if let Ok(path) = document.path.canonicalize() {
                snapshot.files.insert(path.clone(), uri.clone());
                snapshot
                    .overrides
                    .insert(path.clone(), document.text.clone());
                snapshot.paths.insert(uri.clone(), path);
            }
        }
        snapshot
    }
}

fn analyze(
    snapshot: &Snapshot,
    path: &Path,
    checker: &joky::Checker,
    cache: &EditorParseCache,
    stale: impl Fn() -> bool,
) -> Option<Analysis> {
    if stale() {
        return None;
    }
    let mut analysis = Analysis::default();
    let documents = &snapshot.targets[path];
    let inputs = RefCell::new(BTreeSet::new());
    let observe = |file: &Path| {
        inputs.borrow_mut().insert(file.to_owned());
    };
    let sources = EditorSources::new(cache, &snapshot.overrides)
        .with_observer(&observe)
        .with_cancellation(&stale);
    let types = checker.type_registry();
    let project = Project::load_tracked(path, &observe);
    let result = match project.as_ref() {
        Ok(project) => {
            analysis.library_root = project
                .as_ref()
                .filter(|project| project.entry.is_none())
                .map(|project| project.source_root.clone());
            let mut visit = |recover| {
                check::visit_sources(
                    path,
                    project.as_ref(),
                    checker,
                    |project, file| {
                        if stale() {
                            return Err(Error::new_with_code(
                                ErrorPhase::Resolve,
                                joky::ErrorCode::Cancelled,
                                "Editor snapshot was superseded",
                            ));
                        }
                        match project {
                            Some(project) => {
                                project.resolve_editor_snapshot(types, file, sources, recover)
                            }
                            None => types.resolve_editor_snapshot(file, None, sources, recover),
                        }
                    },
                    |ast, validation| {
                        if !stale() {
                            let (index, result) = checker.analyze_module(ast);
                            analysis.indexes.push(index);
                            if !recover {
                                return validation.and(result);
                            }
                        }
                        Ok(())
                    },
                )
            };
            if project.is_none()
                && documents
                    .iter()
                    .all(|uri| !snapshot.paths.contains_key(uri))
            {
                Ok(())
            } else {
                let result = visit(false);
                if result
                    .as_ref()
                    .is_err_and(|error| error.phase() == ErrorPhase::Parse)
                    && !stale()
                {
                    // Retain the strict diagnostic even when semantic recovery succeeds.
                    let _ = visit(true);
                }
                result
            }
        }
        Err(error) => Err(error.clone()),
    };
    if stale() {
        return None;
    }
    if let Err(error) = result {
        if let Some((uri, version, diagnostic)) = diagnostic(&error, snapshot, path) {
            analysis.reports.insert(
                uri,
                Report {
                    version,
                    diagnostic,
                },
            );
        } else {
            analysis.messages.push(format!(
                "{}: [{}] {}",
                path.display(),
                error.code(),
                error.message()
            ));
        }
    }
    let checked = inputs.into_inner();
    for uri in documents {
        let needs_syntax = snapshot
            .paths
            .get(uri)
            .is_none_or(|path| !checked.contains(path))
            || !analysis.messages.is_empty()
            || !analysis.reports.is_empty();
        if needs_syntax && !analysis.reports.contains_key(uri) {
            let document = &snapshot.documents[uri];
            let result = if let Some(path) = snapshot.paths.get(uri) {
                cache.check_syntax(path, document.text.clone())
            } else {
                joky::parser::parse(&document.text).map(|_| ())
            };
            if let Err(error) = result {
                check_syntax(&mut analysis.reports, uri, document, error);
            }
        }
    }
    analysis.inputs = checked;
    (!stale()).then_some(analysis)
}

fn check_syntax(
    reports: &mut BTreeMap<String, Report>,
    uri: &str,
    document: &Document,
    error: joky::parser::Diagnostic,
) {
    if error.span.slice(&document.text).is_none() {
        return;
    }
    let range = json!({"start": position(&document.text, error.span.start()), "end": position(&document.text, error.span.end())});
    reports.insert(uri.to_owned(), Report {
        version: Some(document.version),
        diagnostic: json!({"range": range, "severity": 1, "source": "Joky", "code": error.code.as_str(), "message": error.message}),
    });
}

fn diagnostic(
    error: &Error,
    snapshot: &Snapshot,
    target: &Path,
) -> Option<(String, Option<i64>, Value)> {
    let location = |span: Span| {
        let file = error.sources().get(span.source_id())?;
        let path = file.path()?;
        let (uri, version) = match snapshot.files.get(path) {
            Some(uri) => (uri.clone(), Some(snapshot.documents[uri].version)),
            None => (display_uri(path, target)?, None),
        };
        span.slice(file.text())?;
        let range = source_range(file, span);
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

fn display_uri(path: &Path, target: &Path) -> Option<String> {
    let display_root = target.parent()?;
    let root = display_root.canonicalize().ok();
    let display = root
        .as_ref()
        .and_then(|root| path.strip_prefix(root).ok())
        .map(|relative| display_root.join(relative));
    Url::from_file_path(display.as_deref().unwrap_or(path))
        .ok()
        .map(Into::into)
}

fn source_range(file: &SourceFile, span: Span) -> Value {
    json!({"start": position(file.text(), span.start()), "end": position(file.text(), span.end())})
}

fn offset(text: &str, line: u64, column: u64) -> Option<usize> {
    let content = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut offset = text.len() - content.len();
    let mut line = line;
    while line > 0 {
        let tail = text.get(offset..)?;
        offset += tail.find(['\r', '\n'])?;
        offset += if text[offset..].starts_with("\r\n") {
            2
        } else {
            1
        };
        line -= 1;
    }
    let mut column = column;
    for ch in text[offset..].chars() {
        if column == 0 {
            return Some(offset);
        }
        if matches!(ch, '\r' | '\n') {
            return None;
        }
        column = column.checked_sub(ch.len_utf16() as u64)?;
        offset += ch.len_utf8();
    }
    (column == 0).then_some(offset)
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
