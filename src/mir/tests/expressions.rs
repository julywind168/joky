use super::*;

#[test]
fn lowers_if_to_branch_and_phi() {
    let mir = lower("fn main() { let value = if true 1 else 2 }");
    let blocks = &mir.functions()[0].blocks;
    assert!(matches!(
        blocks[0].terminator,
        Some(MirTerminator::Branch { .. })
    ));
    assert!(blocks.iter().any(|block| block
        .statements
        .iter()
        .any(|statement| matches!(statement, MirStatement::Phi { .. }))));
}

#[test]
fn lowers_logical_operators_to_short_circuit_cfg() {
    let mir = lower("fn main() { let value = true && false || !false }");
    let function = &mir.functions()[0];
    assert!(
        function
            .blocks
            .iter()
            .filter(|block| matches!(block.terminator, Some(MirTerminator::Branch { .. })))
            .count()
            >= 2
    );
    assert!(function.blocks.iter().any(|block| {
        block
            .statements
            .iter()
            .any(|statement| matches!(statement, MirStatement::Phi { .. }))
    }));
    mir.verify().expect("logical CFG should verify");
}

#[test]
fn lowers_loop_break_and_continue_to_gotos() {
    let mir = lower(
        r#"
            fn main() {
                let value = loop {
                    if true {
                        break 1
                    } else {
                        continue
                    }
                }
                while true {
                    break
                }
            }
        "#,
    );
    let terminators = mir.functions()[0]
        .blocks
        .iter()
        .filter_map(|block| block.terminator.as_ref())
        .collect::<Vec<_>>();
    assert!(
        terminators
            .iter()
            .filter(|terminator| matches!(terminator, MirTerminator::Goto { .. }))
            .count()
            >= 4
    );
}

#[test]
fn lowers_task_loop_heads_to_cancellation_polls() {
    let mir = lower("fn main() { branch { loop { } } }");
    let task = mir
        .functions()
        .iter()
        .find(|function| function.is_task)
        .expect("branch must create a task function");
    assert!(task
        .blocks
        .iter()
        .flat_map(|block| &block.statements)
        .any(|statement| matches!(statement, MirStatement::TaskPoll { .. })));
    assert!(task
        .blocks
        .iter()
        .flat_map(|block| &block.statements)
        .any(|statement| matches!(statement, MirStatement::TaskCancelled { .. })));
    mir.verify().expect("task cancellation MIR should verify");
}

#[test]
fn cancellation_path_drops_task_owned_locals() {
    let mir = lower(
        "class Token { let value: Int32 } fn main() { branch { let token = Token(value: 1); loop { } } }",
    );
    let task = mir
        .functions()
        .iter()
        .find(|function| function.is_task)
        .expect("branch must create a task function");
    let cancel_block = task
        .blocks
        .iter()
        .find(|block| {
            block
                .statements
                .iter()
                .any(|statement| matches!(statement, MirStatement::TaskCancelled { .. }))
        })
        .expect("task must have a cancellation return block");
    assert!(cancel_block
        .statements
        .iter()
        .any(|statement| matches!(statement, MirStatement::DropLocal { .. })));
    mir.verify()
        .expect("cancellation path must retain owned-local cleanup");
}

#[test]
fn verifier_rejects_cancellation_block_without_return() {
    let mut mir = lower("fn main() { branch { loop { } } }");
    let task = mir
        .functions
        .iter_mut()
        .find(|function| function.is_task)
        .expect("branch task");
    let cancel_block = task
        .blocks
        .iter_mut()
        .find(|block| {
            block
                .statements
                .iter()
                .any(|statement| matches!(statement, MirStatement::TaskCancelled { .. }))
        })
        .expect("cancellation block");
    cancel_block.terminator = Some(MirTerminator::Unreachable);
    mir.verify()
        .expect_err("cancellation Abort block must return its placeholder");
}

#[test]
fn lowers_function_return_from_core_value() {
    let mir = lower("fn answer() -> Int32 { if true 1 else 2 } fn main() { answer() }");
    assert!(mir.functions()[0]
        .blocks
        .iter()
        .any(|block| matches!(block.terminator, Some(MirTerminator::Return(Some(_))))));
}

#[test]
fn lowers_literals_to_typed_constants() {
    let mir = lower(
        r#"fn main() {
            let text = "hello"
            let number = 42
            let flag = true
            1.5
        }"#,
    );
    let statements = &mir.functions()[0].blocks[0].statements;
    assert!(statements.iter().any(|statement| matches!(
        statement,
        MirStatement::Const {
            value: MirConstant::String(_),
            ..
        }
    )));
    assert!(statements.iter().any(|statement| matches!(
        statement,
        MirStatement::Const {
            value: MirConstant::Integer(42),
            ..
        }
    )));
    assert!(statements.iter().any(|statement| matches!(
        statement,
        MirStatement::Const {
            value: MirConstant::Boolean(true),
            ..
        }
    )));
}

#[test]
fn lowers_name_reads_to_typed_values() {
    let mir = lower(
        "fn answer() -> Int32 {\n                let value = 42\n                value\n            }\n            fn main() { while answer() == 42 { break } }",
    );
    assert!(mir.functions()[0].blocks[0]
        .statements
        .iter()
        .any(|statement| matches!(statement, MirStatement::Read { local, .. } if local.0 == 0)));
    mir.verify().unwrap();
}

#[test]
fn lowers_unary_and_binary_operations_to_typed_values() {
    let mir = lower(
        "fn answer() -> Int32 {\n                let value = 1 + 2\n                -value\n            }\n            fn main() { while answer() < 0 { break } }",
    );
    let statements = &mir.functions()[0].blocks[0].statements;
    assert!(statements
        .iter()
        .any(|statement| matches!(statement, MirStatement::Binary { .. })));
    assert!(statements
        .iter()
        .any(|statement| matches!(statement, MirStatement::Unary { .. })));
    mir.verify().unwrap();
}

#[test]
fn lowers_named_function_calls_to_mir_values() {
    let mir = lower(
        "fn answer() -> Int32 { 42 }\n            fn main() { while answer() == 42 { break } }",
    );
    assert!(mir.functions()[1].blocks.iter().any(|block| {
        block.statements.iter().any(
            |statement| matches!(statement, MirStatement::Call { function, .. } if function.0 == 0),
        )
    }));
    mir.verify().unwrap();
}

#[test]
fn lowers_tuple_and_struct_field_reads_to_projections() {
    let mir = lower(
        "fn answer() -> Int32 {\n                let pair = (1, 2)\n                pair.0\n            }\n            fn main() { while answer() == 1 { break } }",
    );
    assert!(mir.functions()[0].blocks[0]
        .statements
        .iter()
        .any(|statement| matches!(statement, MirStatement::Project { .. })));
    assert!(mir.functions()[0].blocks[0]
        .statements
        .iter()
        .any(|statement| matches!(statement, MirStatement::Tuple { .. })));
    mir.verify().unwrap();
}

#[test]
fn lowers_enum_variants_to_typed_constructors() {
    let mir = lower(
        "enum Shape {\n                Circle(radius: Float32)\n                Triangle\n            }\n            fn main() {\n                let shape = Shape.Circle(radius: 2.0)\n                let other = Shape.Triangle\n            }",
    );
    assert!(mir.functions()[0].blocks.iter().any(|block| {
        block
            .statements
            .iter()
            .any(|statement| matches!(statement, MirStatement::EnumConstruct { .. }))
    }));
    mir.verify().unwrap();
}

#[test]
fn lowers_struct_and_class_constructors_to_mir() {
    let mir = lower(
        "struct Point { let x: Int32, let y: Int32 = 2 }\n\
         class Counter { var value: Int32 = 0 }\n\
         fn main() { let point = Point(x: 1); let counter = Counter(value: 3) }",
    );
    let statements = mir.functions()[0]
        .blocks
        .iter()
        .flat_map(|block| block.statements.iter())
        .collect::<Vec<_>>();
    assert!(statements.iter().any(|statement| matches!(
        statement,
        MirStatement::Construct { type_id, .. } if *type_id == MirTypeId::Struct(0)
    )));
    assert!(statements.iter().any(|statement| matches!(
        statement,
        MirStatement::Construct { type_id, .. } if *type_id == MirTypeId::Class(0)
    )));
    mir.verify().unwrap();
}

#[test]
fn lowers_default_constructor_fields_into_mir_values() {
    let mir = lower(
        "struct Point { let x: Int32 = 1 + 2, let y: Int32 = 4 }\n\
         fn main() { let point = Point() }",
    );
    let statements = mir.functions()[0]
        .blocks
        .iter()
        .flat_map(|block| block.statements.iter())
        .collect::<Vec<_>>();
    assert!(statements
        .iter()
        .any(|statement| matches!(statement, MirStatement::Binary { .. })));
    let fields = statements.iter().find_map(|statement| match statement {
        MirStatement::Construct {
            type_id, fields, ..
        } if *type_id == MirTypeId::Struct(0) => Some(fields),
        _ => None,
    });
    let fields = fields.expect("Point constructor in MIR");
    assert_eq!(fields.len(), 2);
    mir.verify().unwrap();
}

#[test]
fn lowers_match_scrutinees_to_mir() {
    let mir = lower(
        "enum Shape { Circle(radius: Int32), Triangle }\n\
         fn main() {\n\
             let shape = Shape.Circle(radius: 2)\n\
             let size = match shape { Shape.Circle(radius) => radius, Shape.Triangle => 0 }\n\
         }",
    );
    assert!(mir.functions()[0].blocks.iter().any(|block| {
        block
            .statements
            .iter()
            .any(|statement| matches!(statement, MirStatement::EnumTag { .. }))
    }));
    assert!(mir.functions()[0].blocks.iter().any(|block| {
        block
            .statements
            .iter()
            .any(|statement| matches!(statement, MirStatement::EnumProject { .. }))
    }));
    assert!(mir.functions()[0].blocks.iter().any(|block| {
        block
            .statements
            .iter()
            .any(|statement| matches!(statement, MirStatement::Phi { .. }))
    }));
    let dump = mir.dump();
    assert!(dump.contains("enum_tag"));
    assert!(dump.contains("enum_project"));
    assert!(dump.contains("branch"));
    assert!(dump.contains("bind 0"));
    assert!(dump.contains("phi"));
    assert!(!dump.contains("match %"));
    assert!(!dump.contains("bind_pattern"));
    mir.verify().unwrap();
}

#[test]
fn lowers_nested_matches_to_independent_cfgs() {
    let mir = lower(
        "enum Inner { First(value: Int32), Second }\n\
         enum Outer { Some(value: Inner), None }\n\
         fn main() {\n\
             let value = match Outer.Some(value: Inner.First(value: 1)) {\n\
                 Outer.Some(value: Inner.First(value)) => match Inner.First(value: value) {\n\
                     Inner.First(value) => value\n\
                     Inner.Second => 0\n\
                 }\n\
                 Outer.Some(value: Inner.Second) => 0\n\
                 Outer.None => 0\n\
             }\n\
         }",
    );
    let function = &mir.functions()[0];
    let branch_count = function
        .blocks
        .iter()
        .filter(|block| matches!(block.terminator, Some(MirTerminator::Branch { .. })))
        .count();
    assert!(
        branch_count >= 2,
        "expected CFGs for both match expressions"
    );
    assert!(
        function
            .blocks
            .iter()
            .filter(|block| {
                block
                    .statements
                    .iter()
                    .any(|statement| matches!(statement, MirStatement::Phi { .. }))
            })
            .count()
            >= 2,
        "expected independent match result merges"
    );
    mir.verify().unwrap();
}

#[test]
fn lowers_match_break_and_continue_inside_loop() {
    let mir = lower(
        "enum Flag { On, Off }\n\
         fn main() {\n\
             let flag = Flag.On\n\
             loop {\n\
                 match flag {\n\
                     Flag.On => continue\n\
                     Flag.Off => break\n\
                 }\n\
             }\n\
         }",
    );
    let function = &mir.functions()[0];
    let gotos = function
        .blocks
        .iter()
        .filter_map(|block| match block.terminator {
            Some(MirTerminator::Goto { target, .. }) => Some(target),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(gotos.len() >= 4, "expected loop and match transfer edges");
    assert!(
        function.blocks.iter().any(|block| {
            block.statements.is_empty()
                && matches!(block.terminator, Some(MirTerminator::Unreachable))
        }),
        "a match whose arms both transfer control has no reachable merge"
    );
    mir.verify().unwrap();
}

#[test]
fn match_arm_bindings_use_distinct_local_slots() {
    let mir = lower(
        "enum Pair { Left(value: Int32), Right(value: Int32) }\n\
         fn main() {\n\
             let pair = Pair.Left(value: 1)\n\
             let result = match pair {\n\
                 Pair.Left(value) => value\n\
                 Pair.Right(value) => value\n\
             }\n\
         }",
    );
    let function = &mir.functions()[0];
    let binding_locals = function
        .locals
        .iter()
        .filter(|local| local.name == "value")
        .map(|local| local.id)
        .collect::<Vec<_>>();
    assert_eq!(
        binding_locals.len(),
        2,
        "each arm should have its own binding"
    );
    let reads = function
        .blocks
        .iter()
        .flat_map(|block| &block.statements)
        .filter_map(|statement| match statement {
            MirStatement::Read { local, .. } if binding_locals.contains(local) => Some(*local),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(binding_locals.iter().all(|local| reads.contains(local)));
    mir.verify().unwrap();
}

#[test]
fn lowers_complex_match_phis_and_unreachable_fallback() {
    let mir = lower(
        "enum Shape { Circle(radius: Int32), Triangle }\n\
         fn main() {\n\
             let shape = Shape.Circle(radius: 2)\n\
             let value = match shape {\n\
                 Shape.Circle(radius) => if radius > 0 { 1 } else { 2 }\n\
                 Shape.Triangle => if true { 3 } else { 4 }\n\
             }\n\
         }",
    );
    let function = &mir.functions()[0];
    let phi_count = function
        .blocks
        .iter()
        .flat_map(|block| &block.statements)
        .filter(|statement| matches!(statement, MirStatement::Phi { .. }))
        .count();
    assert!(phi_count >= 3, "expected nested branch and match Phi nodes");
    assert!(
        function
            .blocks
            .iter()
            .any(|block| matches!(block.terminator, Some(MirTerminator::Unreachable))),
        "exhaustive match should retain an unreachable fallback block"
    );
    mir.verify().unwrap();
}

#[test]
fn lowers_class_field_assignment_to_store() {
    let mir = lower(
        "class Counter { var value: Int32 = 0\n fn set() { self.value = 1 } }\n\
         fn main() { let counter = Counter(); counter.set() }",
    );
    assert!(mir
        .functions()
        .iter()
        .any(|function| function.blocks.iter().any(|block| {
            block
                .statements
                .iter()
                .any(|statement| matches!(statement, MirStatement::Store { .. }))
        })));
    mir.verify().unwrap();
}

#[test]
fn lowers_method_calls_to_mir() {
    let mir = lower(
        "class Counter { var value: Int32 = 0\n fn set(value: Int32) { self.value = value } }\n\
         fn main() { let counter = Counter(); counter.set(value: 1) }",
    );
    assert!(mir
        .functions()
        .iter()
        .any(|function| function.blocks.iter().any(|block| {
            block
                .statements
                .iter()
                .any(|statement| matches!(statement, MirStatement::MethodCall { .. }))
        })));
    mir.verify().unwrap();
}

#[test]
fn lowers_println_to_runtime_call() {
    let mir = lower("fn main() { println(42) }");
    assert!(mir.functions()[0].blocks.iter().any(|block| {
        block.statements.iter().any(|statement| {
            matches!(
                statement,
                MirStatement::RuntimeCall {
                    intrinsic: RuntimeIntrinsic::Println,
                    ..
                }
            )
        })
    }));
    assert!(mir.functions()[0].blocks.iter().any(|block| {
        block.statements.iter().any(|statement| {
            matches!(
                statement,
                MirStatement::RuntimeCall {
                    intrinsic: RuntimeIntrinsic::Show(Type::I32),
                    ..
                }
            )
        })
    }));
    mir.verify().unwrap();
}

#[test]
fn lowers_print_to_runtime_call() {
    let mir = lower("fn main() { print(42) }");
    assert!(mir.functions()[0].blocks.iter().any(|block| {
        block.statements.iter().any(|statement| {
            matches!(
                statement,
                MirStatement::RuntimeCall {
                    intrinsic: RuntimeIntrinsic::Print,
                    ..
                }
            )
        })
    }));
    mir.verify().unwrap();
}

#[test]
fn lowers_panic_with_a_string_message() {
    let mir = lower("fn main() { panic(\"stop\") }");
    let main = &mir.functions()[0];
    assert!(main.blocks.iter().any(|block| {
        block.statements.iter().any(|statement| {
            matches!(
                statement,
                MirStatement::RuntimeCall {
                    intrinsic: RuntimeIntrinsic::Panic,
                    arguments,
                    ..
                } if arguments.len() == 1
            )
        })
    }));
    assert!(main
        .blocks
        .iter()
        .any(|block| matches!(block.terminator, Some(MirTerminator::Unreachable))));
    mir.verify().unwrap();
}
