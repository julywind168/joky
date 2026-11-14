//! Run completion on a private copy of the current document snapshot.
use super::{position, target, Job, Query};
use crate::cli::project::Project;
use joky::{Checker, CompletionKind, CompletionSite};
use serde_json::{json, Value};

pub(super) fn complete(job: &Job, query: &Query, checker: &Checker) -> Value {
    let items = candidates(job, query, checker).unwrap_or_default();
    json!({"isIncomplete": true, "items": items})
}

fn candidates(job: &Job, query: &Query, checker: &Checker) -> Option<Vec<Value>> {
    let document = job.documents.get(&query.uri)?;
    let path = document.path.canonicalize().ok()?;
    let site = CompletionSite::new(&document.text, query.offset)?;
    let mut overrides = job
        .documents
        .values()
        .filter_map(|document| Some((document.path.canonicalize().ok()?, document.text.clone())))
        .collect::<std::collections::BTreeMap<_, _>>();
    overrides.insert(path.clone(), site.source().into());
    let target = target(&document.path, &job.roots);
    let registry = checker.type_registry();
    let module = match Project::load(&target).ok()? {
        Some(project) => project.resolve_for_editor(registry, &document.path, &overrides),
        None => registry.resolve_file_for_editor(&path, None, &overrides),
    }
    .ok()?;
    let range = json!({
        "start": position(&document.text, site.replacement().start()),
        "end": position(&document.text, site.replacement().end()),
    });
    Some(
        checker
            .complete_module(&module, &path, &site)
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
