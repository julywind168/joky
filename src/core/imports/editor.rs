//! Reusable parses of exact source snapshots for one editor worker.
use crate::{
    parser::{self, ast::Module, Diagnostic},
    SourceFile,
};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

const MAX_FILES: usize = 256;
const MAX_SOURCE_BYTES: usize = 8 * 1024 * 1024;

struct ParsedFile {
    file: SourceFile,
    last_used: u64,
    strict: Option<Result<Module, Diagnostic>>,
    recovered: Option<Result<Module, Diagnostic>>,
}

/// Bounded cache for editor source parsing. The resolver still reads the current text.
#[derive(Default)]
pub struct EditorParseCache {
    files: RefCell<BTreeMap<PathBuf, ParsedFile>>,
    clock: Cell<u64>,
    #[cfg(test)]
    parses: Cell<usize>,
}

impl EditorParseCache {
    /// Check an open file that may not be reachable from the project entry.
    pub fn check_syntax(&self, path: &Path, text: Arc<str>) -> Result<(), Diagnostic> {
        let file = self.source(path, text);
        let mut files = self.files.borrow_mut();
        files
            .get_mut(path)
            .unwrap()
            .strict
            .get_or_insert_with(|| self.parse_source(&file, false))
            .as_ref()
            .map(|_| ())
            .map_err(Clone::clone)
    }

    /// Keep the most recently used files between requests.
    pub fn trim(&self) {
        let mut files = self.files.borrow_mut();
        let mut bytes: usize = files.values().map(|entry| entry.file.text().len()).sum();
        if files.len() <= MAX_FILES && bytes <= MAX_SOURCE_BYTES {
            return;
        }
        let mut order: Vec<_> = files
            .iter()
            .map(|(path, entry)| (entry.last_used, path.clone()))
            .collect();
        order.sort_unstable();
        for (_, path) in order {
            if files.len() <= MAX_FILES && bytes <= MAX_SOURCE_BYTES {
                break;
            }
            bytes -= files.remove(&path).unwrap().file.text().len();
        }
    }

    pub(super) fn source(&self, path: &Path, text: Arc<str>) -> SourceFile {
        let tick = self.clock.get() + 1;
        self.clock.set(tick);
        let mut files = self.files.borrow_mut();
        if let Some(entry) = files
            .get_mut(path)
            .filter(|entry| entry.file.text() == &*text)
        {
            entry.last_used = tick;
            return entry.file.clone();
        }
        let file = SourceFile::from_path(path, path, text);
        files.insert(
            path.to_owned(),
            ParsedFile {
                file: file.clone(),
                last_used: tick,
                strict: None,
                recovered: None,
            },
        );
        file
    }

    pub(super) fn parse(&self, file: &SourceFile, recover: bool) -> Result<Module, Diagnostic> {
        let mut files = self.files.borrow_mut();
        let entry = files.get_mut(file.path().expect("editor file path"));
        let Some(entry) = entry.filter(|entry| entry.file.id() == file.id()) else {
            return self.parse_source(file, recover);
        };
        let strict = entry
            .strict
            .get_or_insert_with(|| self.parse_source(file, false));
        if recover && strict.is_err() {
            entry
                .recovered
                .get_or_insert_with(|| self.parse_source(file, true))
                .clone()
        } else {
            strict.clone()
        }
    }

    fn parse_source(&self, file: &SourceFile, recover: bool) -> Result<Module, Diagnostic> {
        #[cfg(test)]
        self.parses.set(self.parses.get() + 1);
        if recover {
            parser::parse_for_editor(file.text(), file.id())
        } else {
            parser::parse_with_source(file.text(), file.id())
        }
    }
}

/// In-memory file overrides and optional input tracking for one editor request.
#[derive(Clone, Copy)]
pub struct EditorSources<'a> {
    pub(super) cache: &'a EditorParseCache,
    pub(super) buffers: &'a BTreeMap<PathBuf, Arc<str>>,
    pub(super) observe: Option<&'a dyn Fn(&Path)>,
    pub(super) completion: Option<(&'a Path, &'a str)>,
    pub(super) cancelled: Option<&'a dyn Fn() -> bool>,
}

impl<'a> EditorSources<'a> {
    pub fn new(cache: &'a EditorParseCache, buffers: &'a BTreeMap<PathBuf, Arc<str>>) -> Self {
        Self {
            cache,
            buffers,
            observe: None,
            completion: None,
            cancelled: None,
        }
    }

    /// Report paths consulted by resolution, including missing imports.
    pub fn with_observer(mut self, observe: &'a dyn Fn(&Path)) -> Self {
        self.observe = Some(observe);
        self
    }

    /// Stop resolving new files when the request is superseded.
    pub fn with_cancellation(mut self, cancelled: &'a dyn Fn() -> bool) -> Self {
        self.cancelled = Some(cancelled);
        self
    }

    /// Replace a file only for this query. The replacement never enters the cache.
    pub fn with_completion(mut self, path: &'a Path, text: &'a str) -> Self {
        self.completion = Some((path, text));
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::imports::FileOverrides;

    #[test]
    fn editor_parse_cache_shares_dependencies_and_keeps_completion_private() {
        let cache = EditorParseCache::default();
        let entry = Path::new("/main.jk").to_path_buf();
        let dependency = Path::new("/dependency.jk").to_path_buf();
        let mut buffers = BTreeMap::from([
            (entry.clone(), Arc::from("fn main() -> Int32 { 1 }")),
            (
                dependency.clone(),
                Arc::from("pub fn value() -> Int32 { 2 }"),
            ),
        ]);
        let sources = EditorSources::new(&cache, &buffers);
        let strict = FileOverrides::editor(sources, false);
        for path in [&entry, &dependency] {
            let file = strict.read(path, path).unwrap();
            assert_eq!(strict.parse(&file).unwrap().items.len(), 1);
        }
        assert_eq!(cache.parses.get(), 2);
        let completion =
            FileOverrides::editor(sources.with_completion(&entry, "fn main() { }"), true);
        for path in [&entry, &dependency] {
            let file = completion.read(path, path).unwrap();
            assert_eq!(completion.parse(&file).unwrap().items.len(), 1);
        }
        assert_eq!(cache.parses.get(), 2);
        let original = strict.read(&entry, &entry).unwrap();
        assert_eq!(original.text(), "fn main() -> Int32 { 1 }");
        assert!(strict.parse(&original).is_ok());
        assert_eq!(cache.parses.get(), 2);

        buffers.insert(
            dependency.clone(),
            "pub fn value() -> Int32 { let missing = }".into(),
        );
        let sources = EditorSources::new(&cache, &buffers);
        let strict = FileOverrides::editor(sources, false);
        let recover = FileOverrides::editor(sources, true);
        let edited = strict.read(&dependency, &dependency).unwrap();
        for _ in 0..2 {
            assert_eq!(
                strict.parse(&edited).unwrap_err().span.source_id(),
                edited.id()
            );
            assert_eq!(recover.parse(&edited).unwrap().items.len(), 1);
        }
        assert_eq!(cache.parses.get(), 4);
    }
}
