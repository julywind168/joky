//! Lexical bindings and control-flow state used during code generation.

use std::collections::HashMap;

use cranelift_codegen::ir::{FuncRef, Value};

use crate::mir::{MirFunctionId, MirLocalId, MirScopeId, MirTaskId, MirValueId};
use crate::sema::Type;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum CallbackKey {
    Literal(MirFunctionId, MirValueId),
    Retained(usize),
}

#[derive(Clone)]
pub(super) enum CompiledValue {
    Numeric {
        value: Value,
        ty: Type,
    },
    String {
        pointer: Value,
        length: Value,
    },
    Bytes {
        pointer: Value,
    },
    MutBytes {
        pointer: Value,
    },
    NativeHandle {
        pointer: Value,
        ty: Type,
    },
    Boolean {
        value: Value,
    },
    Tuple {
        elements: Vec<CompiledValue>,
        ty: Type,
    },
    Struct {
        fields: Vec<CompiledValue>,
        ty: Type,
    },
    Enum {
        tag: Value,
        fields: Vec<CompiledValue>,
        ty: Type,
    },
    Class {
        pointer: Value,
        ty: Type,
    },
    Cown {
        pointer: Value,
        ty: Type,
    },
    List {
        pointer: Value,
        ty: Type,
    },
    MutList {
        pointer: Value,
        ty: Type,
    },
    MutMap {
        pointer: Value,
        ty: Type,
    },
    MutSet {
        pointer: Value,
        ty: Type,
    },
    Function {
        code: Value,
        ty: Type,
        environment: Value,
    },
    Map {
        pointer: Value,
        ty: Type,
    },
    Unit,
}

#[derive(Clone)]
pub(super) struct FunctionType {
    pub(super) parameters: Vec<Type>,
    pub(super) return_type: Type,
    pub(super) receiver: Option<Type>,
    /// Whether this function may suspend during execution.
    ///
    /// Computed by MIR suspending analysis. Currently used for documentation
    /// and potential future optimizations. The actual suspension mechanism is
    /// handled through effect operations and continuations, not by changes to
    /// the function's ABI or calling convention.
    pub(super) is_suspending: bool,
    /// Pending ABI is enabled for a whole program only after call metadata is
    /// materialized. Keeping this separate preserves the legacy ABI for
    /// programs that have not opted into phase 2 codegen yet.
    pub(super) pending_abi: bool,
    /// True for `@extern` declarations: function-typed parameters flatten to
    /// a single C trampoline address instead of the two-word closure value.
    pub(super) foreign: bool,
}

#[derive(Clone, Copy)]
pub(super) struct UserFunctionRef {
    pub(super) reference: FuncRef,
}

#[derive(Clone)]
pub(super) struct Environment {
    scopes: Vec<HashMap<String, CompiledValue>>,
    locals: HashMap<MirLocalId, CompiledValue>,
    pub(super) function_refs: HashMap<MirFunctionId, UserFunctionRef>,
    pub(super) map_key_refs: HashMap<Type, (FuncRef, FuncRef)>,
    pub(super) closure_call_refs: HashMap<MirFunctionId, FuncRef>,
    pub(super) closure_drop_refs: HashMap<MirFunctionId, FuncRef>,
    /// C-callable trampolines for native callback arguments, keyed by
    /// (extern callee, argument value).
    pub(super) callback_trampoline_refs: HashMap<CallbackKey, FuncRef>,
    pub(super) function_types: HashMap<MirFunctionId, FunctionType>,
    pub(super) class_drop_refs: HashMap<usize, FuncRef>,
    pub(super) managed_drop_ref: Option<FuncRef>,
    pub(super) allocate_ref: Option<FuncRef>,
    pub(super) closure_allocate_ref: Option<FuncRef>,
    /// Runtime task groups carried through continuation CFG edges.
    pub(super) task_groups: HashMap<MirScopeId, Value>,
    pub(super) task_handles: HashMap<MirTaskId, Value>,
}

impl Environment {
    pub(super) fn new() -> Self {
        Self {
            scopes: vec![HashMap::new()],
            locals: HashMap::new(),
            function_refs: HashMap::new(),
            map_key_refs: HashMap::new(),
            closure_call_refs: HashMap::new(),
            closure_drop_refs: HashMap::new(),
            callback_trampoline_refs: HashMap::new(),
            function_types: HashMap::new(),
            class_drop_refs: HashMap::new(),
            managed_drop_ref: None,
            allocate_ref: None,
            closure_allocate_ref: None,
            task_groups: HashMap::new(),
            task_handles: HashMap::new(),
        }
    }

    pub(super) fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    pub(super) fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    pub(super) fn bind_local(&mut self, local: MirLocalId, value: CompiledValue) {
        self.locals.insert(local, value);
    }

    pub(super) fn lookup_local(&self, local: MirLocalId) -> Option<CompiledValue> {
        self.locals.get(&local).cloned()
    }

    /// Lookup used by machine entries. Keeping this API separate makes it
    /// explicit that resumed code may only observe locals reconstructed into
    /// the continuation environment.
    pub(super) fn lookup_local_stackless(&self, local: MirLocalId) -> Option<CompiledValue> {
        self.locals.get(&local).cloned()
    }

    pub(super) fn take_local(&mut self, local: MirLocalId) -> Option<CompiledValue> {
        self.locals.remove(&local)
    }

    pub(super) fn bind_task_group(&mut self, scope: MirScopeId, group: Value) {
        self.task_groups.insert(scope, group);
    }

    pub(super) fn task_group(&self, scope: MirScopeId) -> Option<Value> {
        self.task_groups.get(&scope).copied()
    }

    pub(super) fn bind_task_handle(&mut self, task: MirTaskId, handle: Value) {
        self.task_handles.insert(task, handle);
    }

    /// Closed scopes must not leave branch-local handles in a merged CFG
    /// environment: a later suspend cannot spill those non-dominating values.
    pub(super) fn forget_task_group(
        &mut self,
        scope: MirScopeId,
        function: &crate::mir::MirFunction,
    ) {
        self.task_groups.remove(&scope);
        for statement in function.blocks.iter().flat_map(|block| &block.statements) {
            if let crate::mir::MirStatement::TaskCreate {
                scope: owner, task, ..
            } = statement
            {
                if *owner == scope {
                    self.task_handles.remove(task);
                }
            }
        }
    }

    pub(super) fn task_handle(&self, task: MirTaskId) -> Option<Value> {
        self.task_handles.get(&task).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_scopes_shadow_and_restore_bindings() {
        let mut environment = Environment::new();
        environment.bind_local(MirLocalId(0), CompiledValue::Unit);
        environment.push_scope();
        environment.bind_local(
            MirLocalId(1),
            CompiledValue::Boolean {
                value: Value::from_u32(0),
            },
        );
        assert!(matches!(
            environment.lookup_local(MirLocalId(1)),
            Some(CompiledValue::Boolean { .. })
        ));
        assert!(matches!(
            environment.lookup_local_stackless(MirLocalId(1)),
            Some(CompiledValue::Boolean { .. })
        ));
        environment.pop_scope();
        assert!(matches!(
            environment.lookup_local(MirLocalId(0)),
            Some(CompiledValue::Unit)
        ));
    }

    #[test]
    fn continuation_environment_carries_task_groups_across_cfg_copies() {
        let mut environment = Environment::new();
        let group = Value::from_u32(7);
        environment.bind_task_group(MirScopeId(2), group);
        let copied = environment.clone();
        assert_eq!(copied.task_group(MirScopeId(2)), Some(group));
    }
}
