//! Owned source snapshots for explicit module inputs and durable diagnostics.
use crate::{Error, ErrorCode, ErrorPhase};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

#[derive(
    joky_image_derive::Image, Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
pub struct SourceId(u64);

impl SourceId {
    pub const ANONYMOUS: Self = Self(0);
    pub(crate) const fn raw(self) -> u64 {
        self.0
    }
    pub(crate) const fn from_raw(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceFile {
    id: SourceId,
    name: String,
    text: Arc<str>,
}

// Source text already lives in the embedded source table. The image keeps
// only its key and the ID used by spans in the prepared declarations.
impl crate::core::image::Image for SourceFile {
    #[cfg(joky_bootstrap)]
    fn write(&self, output: &mut crate::core::image::Writer) {
        self.id.write(output);
        self.name.write(output);
    }

    #[cfg(not(joky_bootstrap))]
    fn read(input: &mut crate::core::image::Reader<'_>) -> Self {
        let id = SourceId::read(input);
        let name = String::read(input);
        let text =
            crate::core::imports::embedded_source_text(name.strip_suffix(".jk").unwrap()).into();
        Self { id, name, text }
    }
}

impl SourceFile {
    pub fn new(name: impl Into<String>, text: impl Into<Arc<str>>) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(crate::core::image::FIRST_DYNAMIC_ID);
        Self {
            id: SourceId(
                NEXT.try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
                    .expect("source identity space exhausted"),
            ),
            name: name.into(),
            text: text.into(),
        }
    }
    pub fn id(&self) -> SourceId {
        self.id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn text(&self) -> &str {
        &self.text
    }
}

#[derive(joky_image_derive::Image, Clone, Debug, Default, PartialEq, Eq)]
pub struct SourceMap {
    files: BTreeMap<SourceId, SourceFile>,
}

impl SourceMap {
    pub fn get(&self, id: SourceId) -> Option<&SourceFile> {
        self.files.get(&id)
    }
    pub fn files(&self) -> impl Iterator<Item = &SourceFile> {
        self.files.values()
    }
    pub(crate) fn insert(&mut self, file: SourceFile) {
        self.files.insert(file.id, file);
    }
    pub(crate) fn extend(&mut self, other: &Self) {
        self.files.extend(other.files.clone());
    }
}

/// Explicit in-memory modules. Loading a VM never searches the filesystem.
#[derive(Clone, Debug, Default)]
pub struct ModuleSources {
    modules: BTreeMap<String, SourceFile>,
}

impl ModuleSources {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace a module for the next compilation.
    pub fn insert(
        &mut self,
        path: impl Into<String>,
        source: impl Into<Arc<str>>,
    ) -> Result<(), Error> {
        let path = path.into();
        self.insert_file(path.clone(), SourceFile::new(format!("{path}.jk"), source))
    }

    /// Supply a diagnostic filename independently of the import path.
    pub fn insert_file(&mut self, path: impl Into<String>, file: SourceFile) -> Result<(), Error> {
        let path = path.into();
        validate_module_path(&path)?;
        self.modules.insert(path, file);
        Ok(())
    }

    pub(crate) fn load(&self, path: &str) -> Result<SourceFile, Error> {
        self.modules.get(path).cloned().ok_or_else(|| {
            Error::new_with_code(
                ErrorPhase::Resolve,
                ErrorCode::InvalidImport,
                format!("No source supplied for module '{path}'"),
            )
        })
    }
}

/// Paths under the `joky` namespace name built-in modules; user modules and
/// in-memory sources cannot claim them.
pub(crate) fn is_reserved_module_path(path: &str) -> bool {
    path == "joky" || path.starts_with("joky/")
}

pub(crate) fn validate_module_path(path: &str) -> Result<(), Error> {
    let valid = path.split('/').all(|part| {
        let mut chars = part.chars();
        chars.next().is_some_and(|c| c == '_' || c.is_alphabetic())
            && chars.all(|c| c == '_' || c.is_alphanumeric())
    });
    if !valid || is_reserved_module_path(path) {
        return Err(Error::new_with_code(
            ErrorPhase::Resolve,
            ErrorCode::InvalidImport,
            format!("Invalid or reserved source module path '{path}'"),
        ));
    }
    Ok(())
}
