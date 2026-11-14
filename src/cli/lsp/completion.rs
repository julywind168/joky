//! Run completion on a private copy of the current document snapshot.
use super::{position, Query, Snapshot};
use crate::cli::project::Project;
use joky::{Checker, CompletionKind, CompletionSite, EditorParseCache, EditorSources};
use serde_json::{json, Value};

pub(super) fn complete(
    snapshot: &Snapshot,
    query: &Query,
    checker: &Checker,
    cache: &EditorParseCache,
    stale: impl Fn() -> bool,
) -> Value {
    let items = candidates(snapshot, query, checker, cache, stale).unwrap_or_default();
    json!({"isIncomplete": true, "items": items})
}

fn candidates(
    snapshot: &Snapshot,
    query: &Query,
    checker: &Checker,
    cache: &EditorParseCache,
    stale: impl Fn() -> bool,
) -> Option<Vec<Value>> {
    if stale() {
        return None;
    }
    let document = snapshot.documents.get(&query.uri)?;
    let path = &query.path;
    let site = CompletionSite::new(&document.text, query.offset)?;
    let sources = EditorSources::new(cache, &snapshot.overrides)
        .with_completion(path, site.source())
        .with_cancellation(&stale);
    let registry = checker.type_registry();
    let module = match Project::load(&query.target).ok()? {
        Some(project) => project.resolve_editor_snapshot(registry, &document.path, sources, true),
        None => registry.resolve_editor_snapshot(path, None, sources, true),
    }
    .ok()?;
    if stale() {
        return None;
    }
    let range = json!({
        "start": position(&document.text, site.replacement().start()),
        "end": position(&document.text, site.replacement().end()),
    });
    Some(
        checker
            .complete_module(&module, path, &site)
            .into_iter()
            .map(|item| {
                let kind = match item.kind {
                    CompletionKind::Method => 2,
                    CompletionKind::Function => 3,
                    CompletionKind::Constructor => 4,
                    CompletionKind::Field => 5,
                    CompletionKind::Variable => 6,
                    CompletionKind::Module => 9,
                    CompletionKind::Type => 7,
                    CompletionKind::Constant => 21,
                };
                json!({
                    "label": item.label,
                    "kind": kind,
                    "detail": item.detail,
                    "textEdit": {"range": range, "newText": item.label},
                })
            })
            .collect(),
    )
}
