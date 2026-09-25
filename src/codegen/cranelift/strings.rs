use std::collections::HashSet;

use crate::mir::{MirConstant, MirFunction, MirStatement, MirTerminator};

use super::helpers::{collect_mir_constant_owned_strings, collect_mir_constant_strings};

pub(super) fn collect_program_strings(functions: &[MirFunction]) -> Vec<&[u8]> {
    let mut strings = Vec::new();
    for function in functions {
        // Follow the same CFG order as code generation so literal ordering
        // remains stable when nested branches reorder block ids.
        let mut block_order = Vec::new();
        let mut pending_blocks = vec![function.entry.0];
        let mut seen_blocks = HashSet::new();
        while let Some(block_index) = pending_blocks.pop() {
            if !seen_blocks.insert(block_index) {
                continue;
            }
            block_order.push(block_index);
            match function.blocks[block_index].terminator.as_ref() {
                Some(MirTerminator::Goto { target, .. }) => pending_blocks.push(target.0),
                Some(MirTerminator::Branch {
                    then_block,
                    else_block,
                    ..
                }) => {
                    pending_blocks.push(else_block.0);
                    pending_blocks.push(then_block.0);
                }
                Some(MirTerminator::Return(_) | MirTerminator::Unreachable) | None => {}
            }
        }
        block_order.extend((0..function.blocks.len()).filter(|index| !seen_blocks.contains(index)));
        for block_index in block_order {
            for statement in &function.blocks[block_index].statements {
                match statement {
                    MirStatement::Const {
                        value: MirConstant::String(value),
                        ..
                    } => strings.push(value.as_bytes()),
                    MirStatement::Const {
                        value: MirConstant::Bytes(value),
                        ..
                    } => strings.push(value),
                    MirStatement::HandlerEnter { handlers } => {
                        for handler in handlers {
                            if let Some(value) = handler.resumable_value.as_ref() {
                                collect_mir_constant_strings(value, &mut strings);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    strings
}

pub(super) fn collect_function_strings(function: &MirFunction) -> Vec<Vec<u8>> {
    let mut literals = Vec::new();
    let mut block_order = Vec::new();
    let mut pending_blocks = vec![function.entry.0];
    let mut seen_blocks = HashSet::new();
    while let Some(block_index) = pending_blocks.pop() {
        if !seen_blocks.insert(block_index) {
            continue;
        }
        block_order.push(block_index);
        match function.blocks[block_index].terminator.as_ref() {
            Some(MirTerminator::Goto { target, .. }) => pending_blocks.push(target.0),
            Some(MirTerminator::Branch {
                then_block,
                else_block,
                ..
            }) => {
                pending_blocks.push(else_block.0);
                pending_blocks.push(then_block.0);
            }
            Some(MirTerminator::Return(_) | MirTerminator::Unreachable) | None => {}
        }
    }
    block_order.extend((0..function.blocks.len()).filter(|index| !seen_blocks.contains(index)));
    for block_index in block_order {
        for statement in &function.blocks[block_index].statements {
            match statement {
                MirStatement::Const {
                    value: MirConstant::String(value),
                    ..
                } => literals.push(value.as_bytes().to_vec()),
                MirStatement::Const {
                    value: MirConstant::Bytes(value),
                    ..
                } => literals.push(value.clone()),
                MirStatement::HandlerEnter { handlers } => {
                    for handler in handlers {
                        if let Some(value) = handler.resumable_value.as_ref() {
                            collect_mir_constant_owned_strings(value, &mut literals);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    literals
}
