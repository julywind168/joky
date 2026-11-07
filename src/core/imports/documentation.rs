//! Standard-library documentation uses the compiler's embedded source inventory.
use super::sources::SOURCES;
use crate::{
    parser::documentation::{extract, Documentation},
    Error,
};

pub fn standard_library() -> Result<Vec<(&'static str, Documentation)>, Error> {
    let mut modules = Vec::new();
    for source in SOURCES {
        if source.path.contains("/internal/") {
            continue;
        }
        modules.push((
            source.path,
            extract(source.source, source.kind.exports_unmarked())?,
        ));
    }
    modules.sort_by_key(|(name, _)| *name);
    Ok(modules)
}
