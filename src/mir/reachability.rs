//! Linked-program reachability. Per-module artifacts must keep unused exports.

use std::collections::{HashMap, HashSet};

use super::{MirFunction, MirFunctionId, MirProgram, MirStatement};

pub(super) fn retain_reachable_functions(program: &mut MirProgram) {
    let reachable = reachable_function_ids(program);
    program
        .functions
        .retain(|function| reachable.contains(&function.id));
}

fn is_seed(function: &MirFunction) -> bool {
    (function.receiver.is_none() && function.name == "main")
        || is_compiler_protocol_method(&function.name)
}

/// Codegen can invoke these without a MIR call (map keys, drop glue, debug).
fn is_compiler_protocol_method(name: &str) -> bool {
    matches!(
        name,
        crate::sema::DROP_METHOD
            | crate::sema::HASH_METHOD
            | crate::sema::PARTIAL_EQ_METHOD
            | crate::sema::PARTIAL_ORD_METHOD
            | crate::sema::ORD_METHOD
            | crate::sema::SHOW_METHOD
            | crate::sema::DEBUG_METHOD
            | crate::sema::FROM_STRING_METHOD
            | crate::sema::CURSOR_METHOD
    )
}

fn reachable_function_ids(program: &MirProgram) -> HashSet<MirFunctionId> {
    let functions = program
        .functions
        .iter()
        .map(|function| (function.id, function))
        .collect::<HashMap<_, _>>();
    let mut reachable = HashSet::new();
    let mut stack = Vec::new();
    for function in &program.functions {
        if is_seed(function) {
            stack.push(function.id);
        }
    }
    while let Some(id) = stack.pop() {
        if !reachable.insert(id) {
            continue;
        }
        let Some(function) = functions.get(&id) else {
            continue;
        };
        collect_callees(function, &mut stack);
    }
    reachable
}

fn collect_callees(function: &MirFunction, stack: &mut Vec<MirFunctionId>) {
    for continuation in &function.continuations {
        if let Some(callee) = continuation.callee {
            stack.push(callee);
        }
    }
    for block in &function.blocks {
        for statement in &block.statements {
            match statement {
                MirStatement::Call { function, .. }
                | MirStatement::FunctionValue { function, .. }
                | MirStatement::MethodCall {
                    method: function, ..
                }
                | MirStatement::TaskCreate { function, .. } => stack.push(*function),
                MirStatement::DynamicValue { methods, .. } => {
                    stack.extend(methods.iter().copied());
                }
                MirStatement::HandlerEnter { handlers } => {
                    for handler in handlers {
                        if let Some(function) = handler.resumable_function {
                            stack.push(function);
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hir::CoreProgram;
    use crate::{sema, syntax};

    fn linked(source: &str) -> MirProgram {
        let ast = syntax::parse_program(source).unwrap();
        let types = sema::check_program(&ast).unwrap();
        let core = CoreProgram::lower(ast, types).unwrap();
        MirProgram::lower(&core).unwrap()
    }

    #[test]
    fn drops_unreferenced_helpers_and_keeps_callees() {
        let mut program = linked(
            "
            fn unused() -> Int32 { 2 }
            fn used() -> Int32 { 1 }
            fn main() { println(used()) }
            ",
        );
        retain_reachable_functions(&mut program);
        let names = program
            .functions
            .iter()
            .map(|function| function.name.as_str())
            .collect::<Vec<_>>();
        assert!(names.contains(&"main"));
        assert!(names.contains(&"used"));
        assert!(!names.contains(&"unused"));
        program.verify().unwrap();
    }

    #[test]
    fn keeps_function_values_that_are_not_direct_calls() {
        let mut program = linked(
            "
            fn unused() -> Int32 { 0 }
            fn main() {
                let helper = fn () -> Int32 { 7 }
                println(helper())
            }
            ",
        );
        retain_reachable_functions(&mut program);
        let remaining = program
            .functions
            .iter()
            .map(|function| function.id)
            .collect::<HashSet<_>>();
        let closures = program
            .functions
            .iter()
            .flat_map(|function| &function.blocks)
            .flat_map(|block| &block.statements)
            .filter_map(|statement| match statement {
                MirStatement::FunctionValue { function, .. } => Some(*function),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(!closures.is_empty());
        assert!(closures.iter().all(|id| remaining.contains(id)));
        assert!(!program
            .functions
            .iter()
            .any(|function| function.name == "unused"));
        program.verify().unwrap();
    }

    #[test]
    fn keeps_drop_methods_of_constructed_classes() {
        let mut program = linked(
            "
            class C {}
            impl Drop for C { fn drop(&self) {} }
            fn unused() -> Int32 { 1 }
            fn main() { let _item = C(); }
            ",
        );
        retain_reachable_functions(&mut program);
        assert!(program
            .functions
            .iter()
            .any(|function| function.name == crate::sema::DROP_METHOD));
        assert!(!program
            .functions
            .iter()
            .any(|function| function.name == "unused"));
        program.verify().unwrap();
    }
}
