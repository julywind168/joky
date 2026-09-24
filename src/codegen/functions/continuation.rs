use std::collections::{HashMap, HashSet};

use cranelift_codegen::ir::{types, InstBuilder, Value};
use cranelift_frontend::FunctionBuilder;

use crate::diagnostic::CodegenError;
use crate::mir::{MirBlock, MirFunction, MirLocalId, MirStatement, MirTerminator, MirValueId};
use crate::sema::{Type, TypeTable};

use super::super::abi::abi_types;

pub(super) fn continuation_protocol(
    function: &MirFunction,
    id: crate::mir::MirContinuationId,
) -> Result<crate::mir::MirContinuationKind, CodegenError> {
    function
        .continuations
        .iter()
        .find(|continuation| continuation.id == id)
        .map(|continuation| continuation.kind)
        .ok_or_else(|| CodegenError::RuntimeError {
            message: format!("MIR continuation c{} metadata is missing", id.0),
        })
}

/// Follow the simple value-producing chain used when a resumed local becomes
/// the function result. This identifies the frame slot whose ownership moves
/// into result storage.
pub(super) fn continuation_return_source_local(
    block: &MirBlock,
    mut value: MirValueId,
) -> Option<MirLocalId> {
    loop {
        let source = block
            .statements
            .iter()
            .find_map(|statement| match statement {
                MirStatement::Read {
                    destination, local, ..
                } if *destination == value => Some(Ok(*local)),
                MirStatement::TakeLocal {
                    destination, local, ..
                } if *destination == value => Some(Ok(*local)),
                MirStatement::Dup {
                    destination,
                    value: source,
                }
                | MirStatement::Move {
                    destination,
                    value: source,
                } if *destination == value => Some(Err(*source)),
                _ => None,
            })?;
        match source {
            Ok(local) => return Some(local),
            Err(source) => value = source,
        }
    }
}

pub(super) fn continuation_frame_offset(
    function: &MirFunction,
    continuation: &crate::mir::MirContinuation,
    local: MirLocalId,
    pointer_type: cranelift_codegen::ir::Type,
    types: &TypeTable,
) -> Option<usize> {
    continuation
        .frame_slots
        .iter()
        .any(|slot| slot.local == local)
        .then(|| {
            super::task_frame::LocalFrameLayout::new(function, pointer_type, types).offset(local)
        })
}

pub(super) fn continuation_cleanup_components(
    ty: Type,
    base: usize,
    types: &TypeTable,
) -> Vec<(usize, Type)> {
    match ty {
        Type::String
        | Type::Batch
        | Type::SeqBuilder
        | Type::Hasher
        | Type::Bytes
        | Type::MutBytes
        | Type::Class(_)
        | Type::Native(_)
        | Type::CCallback
        | Type::Cown(_)
        | Type::List(_)
        | Type::MutList(_)
        | Type::Map(_)
        | Type::MutMap(_)
        | Type::MutSet(_) => vec![(base, ty)],
        Type::Function(_) | Type::Dyn(_) => vec![(base + 8, ty)],
        Type::Tuple(id) => {
            let mut offset = base;
            let mut components = Vec::new();
            for element in types.tuple_elements(id) {
                components.extend(continuation_cleanup_components(*element, offset, types));
                offset += abi_types(*element, types::I64, types).len() * 8;
            }
            components
        }
        Type::Struct(id) => {
            let mut offset = base;
            let mut components = Vec::new();
            for (_, field) in types.struct_fields(id) {
                components.extend(continuation_cleanup_components(*field, offset, types));
                offset += abi_types(*field, types::I64, types).len() * 8;
            }
            components
        }
        // Enum-like layouts need tag-aware glue to clean only the active
        // payload. They stay outside this fixed-offset cleanup slice.
        Type::Enum(_) | Type::Option(_) | Type::Result(_) => Vec::new(),
        _ if types.needs_drop(ty) => Vec::new(),
        _ => Vec::new(),
    }
}

pub(super) fn continuation_cleanup_regions(
    ty: Type,
    base: usize,
    types: &TypeTable,
) -> Vec<(usize, Type)> {
    match ty {
        Type::Tuple(id) => {
            let mut offset = base;
            let mut regions = Vec::new();
            for element in types.tuple_elements(id) {
                regions.extend(continuation_cleanup_regions(*element, offset, types));
                offset += abi_types(*element, types::I64, types).len() * 8;
            }
            regions
        }
        Type::Struct(id) => {
            let mut offset = base;
            let mut regions = Vec::new();
            for (_, field) in types.struct_fields(id) {
                regions.extend(continuation_cleanup_regions(*field, offset, types));
                offset += abi_types(*field, types::I64, types).len() * 8;
            }
            regions
        }
        Type::Enum(_) | Type::Option(_) | Type::Result(_) if types.needs_drop(ty) => {
            vec![(base, ty)]
        }
        _ => Vec::new(),
    }
}

pub(super) fn continuation_pointer_offsets(ty: Type, base: usize, types: &TypeTable) -> Vec<usize> {
    match ty {
        Type::String
        | Type::Batch
        | Type::SeqBuilder
        | Type::Hasher
        | Type::Bytes
        | Type::MutBytes
        | Type::Class(_)
        | Type::Native(_)
        | Type::CCallback
        | Type::Cown(_)
        | Type::List(_)
        | Type::MutList(_)
        | Type::Map(_)
        | Type::MutMap(_)
        | Type::MutSet(_) => vec![base],
        Type::Function(_) | Type::Dyn(_) => vec![base + 8],
        Type::Tuple(id) => {
            let mut offset = base;
            let mut pointers = Vec::new();
            for element in types.tuple_elements(id) {
                pointers.extend(continuation_pointer_offsets(*element, offset, types));
                offset += abi_types(*element, types::I64, types).len() * 8;
            }
            pointers
        }
        Type::Struct(id) => {
            let mut offset = base;
            let mut pointers = Vec::new();
            for (_, field) in types.struct_fields(id) {
                pointers.extend(continuation_pointer_offsets(*field, offset, types));
                offset += abi_types(*field, types::I64, types).len() * 8;
            }
            pointers
        }
        Type::Enum(id) => {
            let mut offset = base + 8;
            let mut pointers = Vec::new();
            for variant in types.enum_variants(id) {
                for (_, field) in &variant.fields {
                    pointers.extend(continuation_pointer_offsets(*field, offset, types));
                    offset += abi_types(*field, types::I64, types).len() * 8;
                }
            }
            pointers
        }
        Type::Option(id) => continuation_pointer_offsets(types.option_type(id), base + 8, types),
        Type::Result(id) => {
            let (ok, err) = types.result_types(id);
            let mut pointers = continuation_pointer_offsets(ok, base + 8, types);
            let err_offset = base + 8 + abi_types(ok, types::I64, types).len() * 8;
            pointers.extend(continuation_pointer_offsets(err, err_offset, types));
            pointers
        }
        _ => Vec::new(),
    }
}

pub(super) fn continuation_cleanup_callback(
    builder: &mut FunctionBuilder<'_>,
    ty: Type,
    pointer_type: cranelift_codegen::ir::Type,
    managed_drop_ref: cranelift_codegen::ir::FuncRef,
    class_drop_refs: &HashMap<usize, cranelift_codegen::ir::FuncRef>,
) -> Result<cranelift_codegen::ir::Value, CodegenError> {
    let callback = match ty {
        Type::Class(id) => {
            let drop = class_drop_refs
                .get(&id)
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: format!("class {id} has no drop glue"),
                })?;
            builder.ins().func_addr(pointer_type, *drop)
        }
        _ => builder.ins().func_addr(pointer_type, managed_drop_ref),
    };
    Ok(callback)
}

/// Register the managed leaves of a flattened continuation-owned value. The
/// runtime stores only bytes, so the code generator supplies the exact drop
/// glue and offsets for each operation argument/result layout.
#[allow(clippy::too_many_arguments)]
pub(super) fn register_continuation_cleanups(
    builder: &mut FunctionBuilder<'_>,
    handle: Value,
    storage_kind: u8,
    ty: Type,
    base: usize,
    pointer_type: cranelift_codegen::ir::Type,
    types: &TypeTable,
    register_cleanup_ref: cranelift_codegen::ir::FuncRef,
    register_cleanup_region_ref: cranelift_codegen::ir::FuncRef,
    managed_drop_ref: cranelift_codegen::ir::FuncRef,
    class_drop_refs: &HashMap<usize, cranelift_codegen::ir::FuncRef>,
    tagged_drop_refs: &HashMap<Type, cranelift_codegen::ir::FuncRef>,
) -> Result<(), CodegenError> {
    // Callers omit borrowed operation parameters. Every owned managed value,
    // including native handles, must be released on completion or cancellation.
    for (offset, leaf_type) in continuation_cleanup_components(ty, base, types) {
        let callback = continuation_cleanup_callback(
            builder,
            leaf_type,
            pointer_type,
            managed_drop_ref,
            class_drop_refs,
        )?;
        let storage = builder.ins().iconst(types::I8, storage_kind as i64);
        let offset = builder.ins().iconst(pointer_type, offset as i64);
        builder
            .ins()
            .call(register_cleanup_ref, &[handle, storage, offset, callback]);
    }
    for (offset, tagged_type) in continuation_cleanup_regions(ty, base, types) {
        let drop =
            tagged_drop_refs
                .get(&tagged_type)
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "missing tagged continuation cleanup glue".to_owned(),
                })?;
        let callback = builder.ins().func_addr(pointer_type, *drop);
        let storage = builder.ins().iconst(types::I8, storage_kind as i64);
        let offset = builder.ins().iconst(pointer_type, offset as i64);
        builder.ins().call(
            register_cleanup_region_ref,
            &[handle, storage, offset, callback],
        );
    }
    Ok(())
}

pub(super) fn continuation_machine_blocks(
    function: &MirFunction,
    start: crate::mir::MirBlockId,
) -> Vec<crate::mir::MirBlockId> {
    let mut pending = vec![start];
    let mut visited = HashSet::new();
    let mut blocks = Vec::new();
    while let Some(block) = pending.pop() {
        if !visited.insert(block) || block.0 >= function.blocks.len() {
            continue;
        }
        blocks.push(block);
        // A resumed Suspend installs the next machine entry and returns, so
        // do not compile the next operation's resume edge into this entry.
        if function.blocks[block.0].statements.iter().any(|statement| {
            matches!(
                statement,
                MirStatement::Suspend { .. } | MirStatement::TaskWait { .. }
            )
        }) {
            continue;
        }
        match function.blocks[block.0].terminator.as_ref() {
            Some(MirTerminator::Goto { target, .. }) => pending.push(*target),
            Some(MirTerminator::Branch {
                then_block,
                else_block,
                ..
            }) => {
                pending.push(*else_block);
                pending.push(*then_block);
            }
            Some(MirTerminator::Return(_)) | Some(MirTerminator::Unreachable) | None => {}
        }
    }
    blocks
}

/// The implicit root scope only needs a runtime TaskGroup when lowering a
/// root `branch`. Ordinary functions still carry a synthetic `ScopeExit(s0)`
/// on return, which is deliberately a no-op otherwise.
pub(super) fn function_uses_root_task_scope(function: &MirFunction) -> bool {
    function.blocks.iter().any(|block| {
        block.statements.iter().any(|statement| {
            matches!(
                statement,
                MirStatement::ScopeEnter { scope, .. }
                    | MirStatement::TaskCreate { scope, .. }
                    | MirStatement::TaskJoin { scope, .. }
                    | MirStatement::TaskClaimResult { scope, .. }
                    | MirStatement::TaskCancel { scope, .. }
                    | MirStatement::RaceStart { scope, .. }
                    | MirStatement::RaceSelect { scope, .. }
                    | MirStatement::TaskFailureOperation { scope, .. }
                    | MirStatement::TaskFailurePayload { scope, .. }
                    | MirStatement::TaskFailureClaim { scope }
                    | MirStatement::TaskFailureRethrow { scope, .. }
                    if scope.0 == 0
            )
        })
    })
}

pub(super) fn block_is_reachable(
    blocks: &[crate::mir::MirBlock],
    start: crate::mir::MirBlockId,
    target: crate::mir::MirBlockId,
) -> bool {
    let mut pending = vec![start];
    let mut visited = HashSet::new();
    while let Some(block) = pending.pop() {
        if block == target {
            return true;
        }
        if !visited.insert(block) || block.0 >= blocks.len() {
            continue;
        }
        let Some(terminator) = &blocks[block.0].terminator else {
            continue;
        };
        match terminator {
            MirTerminator::Goto { target, .. } => pending.push(*target),
            MirTerminator::Branch {
                then_block,
                else_block,
                ..
            } => {
                pending.push(*then_block);
                pending.push(*else_block);
            }
            MirTerminator::Return(_) | MirTerminator::Unreachable => {}
        }
    }
    false
}
