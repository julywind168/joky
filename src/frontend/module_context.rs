//! Read-only dependency snapshots scoped to one module compilation session.
use super::*;
use std::sync::Arc;

/// Artifacts remain mutable for linking; import checkers share separate immutable
/// snapshots. Build each layout/interface snapshot only when a cache miss needs semantic analysis,
/// and reuse it for ordinary modules, generic instances and nested type queries.
#[derive(Default)]
pub(super) struct ModuleTypeSnapshots {
    tables: crate::module::DependencyTypes,
}

impl ModuleTypeSnapshots {
    pub(super) fn populate(
        &mut self,
        context: &mut ModuleCompileContext,
        artifacts: &[ModuleArtifact],
    ) {
        // Keep the full available module set: forwarded nominal types and generic
        // arguments can refer to a caller or a transitive dependency, not just the
        // defining source module's direct imports.
        let tables = Arc::make_mut(&mut self.tables);
        // Artifacts only append during compilation. Existing snapshots are a
        // prefix, and completed checkers have released the previous registry.
        for artifact in &artifacts[tables.len()..] {
            tables.insert(
                artifact.metadata.stable_id,
                Arc::new(sema::ModuleTypes {
                    types: artifact.mir.types.clone(),
                    interface: artifact.interface.clone(),
                }),
            );
        }
        context.dependency_types = Arc::clone(&self.tables);
    }
}
