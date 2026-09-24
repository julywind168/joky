use crate::codegen::abi::abi_types;
use crate::mir::{MirFunction, MirLocalId, MirScopeId, MirStatement, MirTaskId};
use crate::sema::TypeTable;

/// A reused continuation keeps one typed location for each local across every
/// suspension. Cleanup descriptors must also cover locals created after entry.
pub(super) struct LocalFrameLayout {
    pub(super) slots: Vec<(MirLocalId, usize)>,
    pub(super) size: usize,
}

impl LocalFrameLayout {
    pub(super) fn new(
        function: &MirFunction,
        pointer_type: cranelift_codegen::ir::Type,
        types: &TypeTable,
    ) -> Self {
        let mut locals = function
            .continuations
            .iter()
            .flat_map(|c| c.frame_slots.iter().map(|s| s.local))
            .collect::<Vec<_>>();
        locals.sort_by_key(|local| local.0);
        locals.dedup();
        let mut size = 0;
        let slots = locals
            .into_iter()
            .map(|local| {
                let offset = size;
                size += abi_types(function.locals[local.0].ty, pointer_type, types).len() * 8;
                (local, offset)
            })
            .collect();
        Self { slots, size }
    }

    pub(super) fn offset(&self, local: MirLocalId) -> usize {
        self.slots
            .iter()
            .find(|(id, _)| *id == local)
            .expect("local has a frame slot")
            .1
    }
}

/// A fixed tail after the local frame, shared by all resume entries
/// of one function. Group slots own heap task storage until ScopeExit.
pub(super) struct TaskFrameLayout {
    pub(super) base: usize,
    pub(super) scopes: Vec<MirScopeId>,
    pub(super) tasks: Vec<MirTaskId>,
}

impl TaskFrameLayout {
    pub(super) fn new(
        function: &MirFunction,
        pointer_type: cranelift_codegen::ir::Type,
        types: &TypeTable,
    ) -> Self {
        let base = LocalFrameLayout::new(function, pointer_type, types).size;
        let mut scopes = Vec::new();
        let mut tasks = Vec::new();
        for statement in function.blocks.iter().flat_map(|block| &block.statements) {
            match statement {
                MirStatement::ScopeEnter { scope, .. } => scopes.push(*scope),
                MirStatement::TaskCreate { scope, task, .. } => {
                    scopes.push(*scope);
                    tasks.push(*task);
                }
                _ => {}
            }
        }
        scopes.sort_by_key(|scope| scope.0);
        scopes.dedup();
        tasks.sort_by_key(|task| task.0);
        tasks.dedup();
        Self {
            base,
            scopes,
            tasks,
        }
    }

    pub(super) fn size(&self) -> usize {
        self.base + (self.scopes.len() + self.tasks.len()) * 8
    }

    pub(super) fn scope_offset(&self, scope: MirScopeId) -> Option<i32> {
        self.scopes
            .iter()
            .position(|id| *id == scope)
            .map(|index| (self.base + index * 8) as i32)
    }

    pub(super) fn task_offset(&self, task: MirTaskId) -> Option<i32> {
        self.tasks
            .iter()
            .position(|id| *id == task)
            .map(|index| (self.base + (self.scopes.len() + index) * 8) as i32)
    }
}
