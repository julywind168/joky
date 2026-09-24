use super::*;

mod ownership;

#[test]
fn verifier_rejects_unterminated_reachable_blocks() {
    let mut mir = lower("fn main() { 1 }");
    mir.functions[0].blocks[0].terminator = None;
    assert!(mir.verify().is_err());
}

#[test]
fn dump_renders_cfg_values_and_edges() {
    let dump = lower("fn main() { let value = if true 1 else 2 }").dump();
    assert!(dump.contains("bb0:"));
    assert!(dump.contains("branch"));
    assert!(dump.contains("phi"));
    assert!(dump.contains("Int32"));
}

#[test]
fn verifier_rejects_mismatched_phi_edge_arguments() {
    let mut mir = lower("fn main() { let value = if true 1 else 2 }");
    let function = &mut mir.functions[0];
    for block in &mut function.blocks {
        if let Some(MirTerminator::Goto { arguments, .. }) = block.terminator.as_mut() {
            if !arguments.is_empty() {
                arguments.clear();
                break;
            }
        }
    }
    assert!(mir.verify().is_err());
}

#[test]
fn verifier_rejects_non_boolean_branch_conditions() {
    let mut mir = lower("fn main() { if true 1 else 2 }");
    let function = &mut mir.functions[0];
    let condition = match function.blocks[0].terminator {
        Some(MirTerminator::Branch { condition, .. }) => condition,
        _ => panic!("expected a branch terminator"),
    };
    function.value_types[condition.0] = Type::I32;
    assert!(mir.verify().is_err());
}

#[test]
fn verifier_rejects_invalid_phi_predecessors_with_task_results() {
    let source = "fn main() {\n\
             let result = parallel {\n| 1\n}\n\
             let value = if true result.0 else 2\n\
         }";
    lower(source)
        .verify()
        .expect("valid task result and phi edges");
    for case in ["out of bounds", "unreachable", "not a predecessor"] {
        let mut mir = lower(source);
        let function = mir
            .functions
            .iter_mut()
            .find(|function| function.name == "main")
            .unwrap();
        let invalid = match case {
            "out of bounds" => MirBlockId(function.blocks.len() + 10),
            "unreachable" => {
                let id = MirBlockId(function.blocks.len());
                function.blocks.push(MirBlock {
                    id,
                    scoped: false,
                    scope_depth: 0,
                    statements: Vec::new(),
                    terminator: Some(MirTerminator::Unreachable),
                });
                id
            }
            _ => function.entry,
        };
        let incoming = function
            .blocks
            .iter_mut()
            .flat_map(|block| &mut block.statements)
            .find_map(|statement| match statement {
                MirStatement::Phi { incoming, .. } => Some(incoming),
                _ => None,
            })
            .expect("if expression has a phi");
        incoming[0].0 = invalid;
        let error = mir.verify().expect_err(case);
        assert!(
            error.to_string().contains("invalid Phi predecessor"),
            "{case}: {error}"
        );
    }
}

#[test]
fn verifier_rejects_use_of_value_from_non_dominating_block() {
    let mut mir = lower("fn main() { let value = if true 1 else 2 }");
    let function = &mut mir.functions[0];

    // Find the then-branch block and its Const value.
    let then_block = match &function.blocks[0].terminator {
        Some(MirTerminator::Branch { then_block, .. }) => *then_block,
        _ => panic!("expected a branch terminator"),
    };
    let then_value = function.blocks[then_block.0]
        .statements
        .iter()
        .find_map(|s| {
            if let MirStatement::Const { destination, .. } = s {
                Some(*destination)
            } else {
                None
            }
        })
        .expect("then-block has a Const value");

    // Find the merge block (has a Phi) and replace the Bind's value
    // with the then-block value, which does NOT dominate the merge block.
    for block in &mut function.blocks {
        let has_phi = block
            .statements
            .iter()
            .any(|s| matches!(s, MirStatement::Phi { .. }));
        if !has_phi {
            continue;
        }
        for statement in &mut block.statements {
            if let MirStatement::Bind {
                value: Some(ref mut v),
                ..
            } = statement
            {
                *v = then_value;
            }
        }
    }
    assert!(mir.verify().is_err());
}

#[test]
fn verifier_accepts_value_defined_in_dominating_block() {
    // In a straight-line block, the first block dominates all later blocks,
    // so using a value from an earlier block is valid.
    let mir = lower("fn main() { let x = 1; let y = x }");
    mir.verify().unwrap();
}

#[test]
fn verifier_allows_owned_values_to_be_recreated_on_loop_back_edges() {
    let mir = lower(
        "class Token {}\n\
         fn consume(value: Token) {}\n\
         fn run(flag: Bool) -> Unit {\n\
             loop {\n\
                 let token = Token()\n\
                 consume(token)\n\
                 if flag { continue } else { break }\n\
             }\n\
         }\n\
         fn main() { run(false) }",
    );
    mir.verify().unwrap();
}

#[test]
fn verifier_accepts_phi_value_from_both_branches() {
    // Phi values correctly merge definitions from both branches.
    let mir = lower("fn main() { let value = if true 1 else 2 }");
    mir.verify().unwrap();
}

#[test]
fn verifier_accepts_valid_tuple_projection() {
    let mir = lower("fn f() -> Int32 { let t = (1, 2); t.0 } fn main() { f() }");
    verify_one(&mir, 0).unwrap();
}

#[test]
fn verifier_accepts_valid_struct_and_class_projection() {
    let mir = lower(
        "struct S { let a: Int32 = 1 } class C { var a: Int32 = 2 } fn f() -> Int32 { let s = S(); let c = C(); s.a + c.a } fn main() { f() }",
    );
    verify_one(&mir, 0).unwrap();
}

#[test]
fn verifier_rejects_out_of_bounds_tuple_projection() {
    let mut mir = lower("fn f() -> Int32 { let t = (1, 2); t.0 } fn main() { f() }");
    let mut found = false;
    {
        let function = &mut mir.functions[0];
        for block in &mut function.blocks {
            for statement in &mut block.statements {
                if let MirStatement::Project { access, .. } = statement {
                    *access = MirFieldAccess::Index(99);
                    found = true;
                }
            }
        }
    }
    assert!(found, "expected a tuple projection");
    assert!(verify_one(&mir, 0).is_err());
}

#[test]
fn verifier_rejects_unknown_struct_field_projection() {
    let mut mir = lower(
        "struct S { let a: Int32 = 1 } fn f() -> Int32 { let s = S(); s.a } fn main() { f() }",
    );
    let mut found = false;
    {
        let function = &mut mir.functions[0];
        for block in &mut function.blocks {
            for statement in &mut block.statements {
                if let MirStatement::Project { access, .. } = statement {
                    *access = MirFieldAccess::Index(99);
                    found = true;
                }
            }
        }
    }
    assert!(found, "expected a struct projection");
    assert!(verify_one(&mir, 0).is_err());
}

#[test]
fn verifier_rejects_unknown_class_field_projection() {
    let mut mir = lower(
        "class C { var a: Int32 = 1 } fn f() -> Int32 { let c = C(); c.a } fn main() { f() }",
    );
    let mut found = false;
    {
        let function = &mut mir.functions[0];
        for block in &mut function.blocks {
            for statement in &mut block.statements {
                if let MirStatement::Project { access, .. } = statement {
                    *access = MirFieldAccess::Index(99);
                    found = true;
                }
            }
        }
    }
    assert!(found, "expected a class projection");
    assert!(verify_one(&mir, 0).is_err());
}

#[test]
fn verifier_rejects_projection_on_non_aggregate_type() {
    let mut mir = lower("fn f() -> Int32 { let x = 1; x } fn main() { f() }");
    {
        let function = &mut mir.functions[0];
        // Find an integer value and use it as the base of a Project.
        let base = function.blocks[0]
            .statements
            .iter()
            .find_map(|statement| {
                if let MirStatement::Const {
                    destination,
                    value: MirConstant::Integer(_),
                } = statement
                {
                    Some(*destination)
                } else {
                    None
                }
            })
            .expect("program has an integer constant");
        let destination = MirValueId(function.value_types.len());
        function.value_types.push(Type::I32);
        function.blocks[0].statements.insert(
            1,
            MirStatement::Project {
                destination,
                base,
                access: MirFieldAccess::Index(99),
            },
        );
    }
    assert!(verify_one(&mir, 0).is_err());
}

#[test]
fn verifier_accepts_valid_enum_construct_and_project() {
    let mir = lower(
        "enum Shape { Circle(radius: Int32), Triangle }\n\
         fn main() {\n\
             let shape = Shape.Circle(radius: 2)\n\
             let size = match shape { Shape.Circle(radius) => radius, Shape.Triangle => 0 }\n\
         }",
    );
    mir.verify().unwrap();
}

#[test]
fn verifier_rejects_unknown_enum_construct_variant() {
    let mut mir = lower(
        "enum Shape { Circle(radius: Int32), Triangle }\n\
         fn main() {\n\
             let shape = Shape.Circle(radius: 2)\n\
             let size = match shape { Shape.Circle(radius) => radius, Shape.Triangle => 0 }\n\
         }",
    );
    let mut found = false;
    {
        let function = &mut mir.functions[0];
        for block in &mut function.blocks {
            for statement in &mut block.statements {
                if let MirStatement::EnumConstruct { variant, .. } = statement {
                    *variant = usize::MAX;
                    found = true;
                }
            }
        }
    }
    assert!(found, "expected an enum construct");
    assert!(mir.verify().is_err());
}

#[test]
fn verifier_rejects_too_many_enum_construct_arguments() {
    let mut mir = lower(
        "enum Shape { Circle(radius: Int32), Triangle }\n\
         fn main() {\n\
             let shape = Shape.Circle(radius: 2)\n\
             let size = match shape { Shape.Circle(radius) => radius, Shape.Triangle => 0 }\n\
         }",
    );
    let mut found = false;
    {
        let function = &mut mir.functions[0];
        for block in &mut function.blocks {
            for statement in &mut block.statements {
                if let MirStatement::EnumConstruct { arguments, .. } = statement {
                    arguments.push(MirCallArgument {
                        parameter: 0,
                        value: MirValueId(0),
                    });
                    found = true;
                    break;
                }
            }
            if found {
                break;
            }
        }
    }
    assert!(found, "expected an enum construct");
    assert!(mir.verify().is_err());
}

#[test]
fn verifier_rejects_out_of_bounds_enum_project_variant() {
    let mut mir = lower(
        "enum Shape { Circle(radius: Int32), Triangle }\n\
         fn main() {\n\
             let shape = Shape.Circle(radius: 2)\n\
             let size = match shape { Shape.Circle(radius) => radius, Shape.Triangle => 0 }\n\
         }",
    );
    let mut found = false;
    {
        let function = &mut mir.functions[0];
        for block in &mut function.blocks {
            for statement in &mut block.statements {
                if let MirStatement::EnumProject { variant, .. } = statement {
                    *variant = 99;
                    found = true;
                }
            }
        }
    }
    assert!(found, "expected an enum projection");
    assert!(mir.verify().is_err());
}

#[test]
fn verifier_rejects_out_of_bounds_enum_project_field() {
    let mut mir = lower(
        "enum Shape { Circle(radius: Int32), Triangle }\n\
         fn main() {\n\
             let shape = Shape.Circle(radius: 2)\n\
             let size = match shape { Shape.Circle(radius) => radius, Shape.Triangle => 0 }\n\
         }",
    );
    let mut found = false;
    {
        let function = &mut mir.functions[0];
        for block in &mut function.blocks {
            for statement in &mut block.statements {
                if let MirStatement::EnumProject { field, .. } = statement {
                    *field = 99;
                    found = true;
                }
            }
        }
    }
    assert!(found, "expected an enum projection");
    assert!(mir.verify().is_err());
}

#[test]
fn verifier_accepts_valid_tuple_construction() {
    let mir = lower("fn f() -> Int32 { let t = (1, 2); t.0 } fn main() { f() }");
    verify_one(&mir, 0).unwrap();
}

#[test]
fn verifier_rejects_tuple_element_count_mismatch() {
    let mut mir = lower("fn f() -> Int32 { let t = (1, 2); t.0 } fn main() { f() }");
    let mut found = false;
    {
        let function = &mut mir.functions[0];
        for block in &mut function.blocks {
            for statement in &mut block.statements {
                if let MirStatement::Tuple { elements, .. } = statement {
                    // Duplicate the first element to make the count wrong.
                    let first = elements[0];
                    elements.push(first);
                    found = true;
                }
            }
        }
    }
    assert!(found, "expected a tuple construction");
    assert!(verify_one(&mir, 0).is_err());
}

#[test]
fn verifier_rejects_tuple_element_type_mismatch() {
    let mut mir =
        lower("fn f() -> Int32 { let s = \"abc\"; let t = (1, 2); t.0 } fn main() { f() }");
    let string_value = {
        let function = &mut mir.functions[0];
        function.blocks[0]
            .statements
            .iter()
            .find_map(|statement| {
                if let MirStatement::Const {
                    destination,
                    value: MirConstant::String(_),
                } = statement
                {
                    Some(*destination)
                } else {
                    None
                }
            })
            .expect("program has a String constant")
    };
    let mut found = false;
    {
        let function = &mut mir.functions[0];
        for block in &mut function.blocks {
            for statement in &mut block.statements {
                if let MirStatement::Tuple { elements, .. } = statement {
                    elements[0] = string_value;
                    found = true;
                }
            }
        }
    }
    assert!(found, "expected a tuple construction");
    assert!(verify_one(&mir, 0).is_err());
}

#[test]
fn verifier_accepts_valid_free_function_call() {
    let mir = lower(
        "fn add(a: Int32, b: Int32) -> Int32 { a + b }\n\
         fn f() -> Int32 { add(1, 2) }\n\
         fn main() { let x = add(1, 2) }",
    );
    mir.verify().unwrap();
}

#[test]
fn lowers_labeled_and_positional_arguments_to_parameter_indices() {
    let mir = lower(
        "fn add(a: Int32, b: Int32) -> Int32 { a + b }\n\
         fn main() { let x = add(b: 2, 1) }",
    );
    let arguments = mir.functions()[1]
        .blocks
        .iter()
        .flat_map(|block| &block.statements)
        .find_map(|statement| {
            if let MirStatement::Call { arguments, .. } = statement {
                Some(
                    arguments
                        .iter()
                        .map(|argument| argument.parameter)
                        .collect::<Vec<_>>(),
                )
            } else {
                None
            }
        })
        .expect("expected a function call");
    assert_eq!(arguments, [1, 0]);
    mir.verify().unwrap();
}

#[test]
fn verifier_accepts_valid_method_call() {
    let mir = lower(
        "class C { var value: Int32 = 0\n fn get() -> Int32 { value } }\n\
         fn f() -> Int32 { let c = C(); c.get() }\n\
         fn main() { let x = C().get() }",
    );
    mir.verify().unwrap();
}

#[test]
fn verifier_rejects_call_to_unknown_function() {
    let mut mir = lower(
        "fn add(a: Int32, b: Int32) -> Int32 { a + b }\n\
         fn f() -> Int32 { add(1, 2) }\n\
         fn main() { let x = add(1, 2) }",
    );
    let mut found = false;
    {
        for function in &mut mir.functions {
            for block in &mut function.blocks {
                for statement in &mut block.statements {
                    if let MirStatement::Call { function: name, .. } = statement {
                        *name = MirFunctionId(usize::MAX);
                        found = true;
                    }
                }
            }
        }
    }
    assert!(found, "expected a free function call");
    assert!(mir.verify().is_err());
}

#[test]
fn verifier_rejects_call_with_too_few_arguments() {
    let mut mir = lower(
        "fn add(a: Int32, b: Int32) -> Int32 { a + b }\n\
         fn f() -> Int32 { add(1, 2) }\n\
         fn main() { let x = add(1, 2) }",
    );
    let mut found = false;
    {
        for function in &mut mir.functions {
            for block in &mut function.blocks {
                for statement in &mut block.statements {
                    if let MirStatement::Call {
                        arguments,
                        function,
                        ..
                    } = statement
                    {
                        if function.0 == 0 {
                            arguments.pop();
                            found = true;
                        }
                    }
                }
            }
        }
    }
    assert!(found, "expected a call to 'add'");
    assert!(mir.verify().is_err());
}

#[test]
fn verifier_rejects_call_with_wrong_argument_type() {
    let mut mir = lower(
        "fn add(a: Int32, b: Int32) -> Int32 { a + b }\n\
         fn main() { let s = \"x\"; let x = add(1, 2) }",
    );
    let string_value = {
        let function = mir.functions.iter().find(|f| f.name == "main").unwrap();
        function.blocks[0]
            .statements
            .iter()
            .find_map(|statement| {
                if let MirStatement::Const {
                    destination,
                    value: MirConstant::String(_),
                } = statement
                {
                    Some(*destination)
                } else {
                    None
                }
            })
            .expect("program has a String constant")
    };
    let mut found = false;
    {
        let funcs = mir.functions.as_mut_slice();
        for function in funcs.iter_mut().filter(|f| f.name == "main") {
            for block in &mut function.blocks {
                for statement in &mut block.statements {
                    if let MirStatement::Call {
                        arguments,
                        function,
                        ..
                    } = statement
                    {
                        if function.0 == 0 {
                            arguments[0].value = string_value;
                            found = true;
                        }
                    }
                }
            }
        }
    }
    assert!(found, "expected a call to 'add'");
    assert!(mir.verify().is_err());
}

#[test]
fn verifier_rejects_call_to_unknown_method() {
    let mut mir = lower(
        "class C { var value: Int32 = 0\n fn get() -> Int32 { value } }\n\
         fn main() { let x = C().get() }",
    );
    let mut found = false;
    {
        for function in &mut mir.functions {
            for block in &mut function.blocks {
                for statement in &mut block.statements {
                    if let MirStatement::MethodCall { method, .. } = statement {
                        *method = MirFunctionId(usize::MAX);
                        found = true;
                    }
                }
            }
        }
    }
    assert!(found, "expected a method call");
    assert!(mir.verify().is_err());
}

#[test]
fn verifier_rejects_integer_constant_with_non_integer_type() {
    let mut mir = lower("fn f() -> Int32 { 1 } fn main() {}");
    let destination = {
        let function = &mir.functions[0];
        match function.blocks[0].statements[0] {
            MirStatement::Const { destination, .. } => destination,
            _ => panic!("expected a Const statement"),
        }
    };
    mir.functions[0].value_types[destination.0] = Type::Bool;
    assert!(mir.verify().is_err());
}

#[test]
fn verifier_rejects_float_integer_cast_operands_and_results() {
    for change_operand in [true, false] {
        let mut mir = lower("fn main() { let converted = 255 as% Int8; }");
        let function = mir.functions.iter_mut().find(|f| f.name == "main").unwrap();
        let (destination, operand) = function
            .blocks
            .iter()
            .flat_map(|block| &block.statements)
            .find_map(|statement| match statement {
                MirStatement::Numeric {
                    destination,
                    method: NumericMethod::IntegerCast,
                    arguments,
                } => Some((*destination, arguments[0])),
                _ => None,
            })
            .unwrap();
        if change_operand {
            function.value_types[operand.0] = Type::F64;
            for statement in function
                .blocks
                .iter_mut()
                .flat_map(|block| &mut block.statements)
            {
                if let MirStatement::Const { destination, value } = statement {
                    if *destination == operand {
                        *value = MirConstant::Float(255.0);
                    }
                }
            }
        } else {
            function.value_types[destination.0] = Type::F64;
        }
        let error = mir
            .verify()
            .expect_err("integer casts cannot contain float types");
        assert!(
            error.message().contains("invalid numeric method types"),
            "{error}"
        );
    }
}

#[test]
fn verifier_rejects_unary_operand_type_mismatch() {
    let mut mir = lower("fn f() -> Int32 { -3 } fn main() {}");
    let operand = {
        let function = &mir.functions[0];
        match function.blocks[0].statements.iter().find_map(|statement| {
            if let MirStatement::Unary { operand, .. } = statement {
                Some(*operand)
            } else {
                None
            }
        }) {
            Some(operand) => operand,
            None => panic!("expected a Unary statement"),
        }
    };
    mir.functions[0].value_types[operand.0] = Type::I64;
    assert!(mir.verify().is_err());
}

#[test]
fn verifier_rejects_binary_operand_type_mismatch() {
    let mut mir = lower("fn f() -> Int32 { 1 + 2 } fn main() {}");
    let left = {
        let function = &mir.functions[0];
        match function.blocks[0].statements.iter().find_map(|statement| {
            if let MirStatement::Binary { left, .. } = statement {
                Some(*left)
            } else {
                None
            }
        }) {
            Some(left) => left,
            None => panic!("expected a Binary statement"),
        }
    };
    mir.functions[0].value_types[left.0] = Type::I64;
    assert!(mir.verify().is_err());
}

#[test]
fn verifier_rejects_constructor_field_count_mismatch() {
    let mut mir = lower(
        "struct S { let a: Int32 = 1, let b: Int32 = 2 }\n\
         fn main() { let s = S(a: 3, b: 4); let _ = s.a }",
    );
    let mut found = false;
    {
        let function = &mut mir.functions[0];
        for block in &mut function.blocks {
            for statement in &mut block.statements {
                if let MirStatement::Construct { fields, .. } = statement {
                    fields.pop();
                    found = true;
                }
            }
        }
    }
    assert!(found, "expected a struct construction");
    assert!(mir.verify().is_err());
}

#[test]
fn verifier_rejects_store_to_non_class_field() {
    let mut mir = lower(
        "class C { var value: Int32 = 0\n fn set() { self.value = 1 } }\n\
         fn main() { let c = C(); c.set() }",
    );
    let mut found = false;
    {
        for function in &mut mir.functions {
            for block in &mut function.blocks {
                for statement in &mut block.statements {
                    if let MirStatement::Store { access, .. } = statement {
                        *access = MirFieldAccess::Index(usize::MAX);
                        found = true;
                    }
                }
            }
        }
    }
    assert!(found, "expected a store statement");
    assert!(mir.verify().is_err());
}

#[test]
fn verifier_rejects_rintln_with_bad_argument_count() {
    let mut mir = lower("fn main() { println(\"hi\") }");
    let mut found = false;
    {
        let function = &mut mir.functions[0];
        for statement in &mut function.blocks[0].statements {
            if let MirStatement::RuntimeCall { arguments, .. } = statement {
                arguments.push(MirCallArgument {
                    parameter: 0,
                    value: MirValueId(0),
                });
                found = true;
            }
        }
    }
    assert!(found, "expected a runtime call");
    assert!(mir.verify().is_err());
}

#[test]
fn verifier_rejects_return_type_mismatch() {
    let mut mir = lower("fn f() -> Int32 { 1 } fn main() {}");
    let value = {
        let function = &mir.functions[0];
        match function.blocks[0].terminator {
            Some(MirTerminator::Return(Some(value))) => value,
            _ => panic!("expected a return terminator"),
        }
    };
    mir.functions[0].value_types[value.0] = Type::I64;
    assert!(mir.verify().is_err());
}

#[test]
fn verifier_rejects_dup_on_owned_class() {
    let mut mir = lower("class C { let value: Int32 = 1 } fn make() -> C { C() } fn main() {}");
    let (source, ty) = mir.functions[0]
        .blocks
        .iter()
        .flat_map(|block| block.statements.iter())
        .find_map(|statement| match statement {
            MirStatement::Construct {
                destination,
                type_id: MirTypeId::Class(id),
                ..
            } => Some((*destination, Type::Class(*id))),
            _ => None,
        })
        .expect("expected class construction");
    let duplicate = MirValueId(mir.functions[0].value_types.len());
    mir.functions[0].value_types.push(ty);
    mir.functions[0].value_ownership.push(MirOwnership::Shared);
    let dropped = MirValueId(mir.functions[0].value_types.len());
    mir.functions[0].value_types.push(Type::Unit);
    mir.functions[0].value_ownership.push(MirOwnership::Copy);
    let statements = &mut mir.functions[0].blocks[0].statements;
    statements.push(MirStatement::Dup {
        destination: duplicate,
        value: source,
    });
    statements.push(MirStatement::Drop {
        destination: dropped,
        value: duplicate,
    });
    assert!(mir.verify().is_err());
}

#[test]
fn lowers_class_local_drop_before_return() {
    let mir = lower("class C { let value: Int32 = 1 } fn main() { let c = C() }");
    let main = mir
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("expected main function");
    assert!(main.blocks.iter().any(|block| {
        matches!(block.terminator, Some(MirTerminator::Return(_)))
            && block
                .statements
                .iter()
                .any(|statement| matches!(statement, MirStatement::DropLocal { .. }))
    }));
    assert!(mir.verify().is_ok());
}

#[test]
fn lowers_class_drops_on_scope_exiting_edges() {
    let mir = lower(
        "class C { let value: Int32 = 1 }\n\
         fn main() { if true { let c = C() } else { let d = C() } }",
    );
    let main = mir
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("expected main function");
    let scoped_drops = main
        .blocks
        .iter()
        .filter(|block| block.scope_depth > 0)
        .filter(|block| {
            block
                .statements
                .iter()
                .any(|statement| matches!(statement, MirStatement::DropLocal { .. }))
        })
        .count();
    assert_eq!(scoped_drops, 2, "each branch must drop its local class");
    assert!(mir.verify().is_ok());
}

#[test]
fn lowers_class_drop_when_breaking_out_of_nested_scope() {
    let mir = lower(
        "class C { let value: Int32 = 1 }\n\
         fn main() { while true { let c = C(); break } }",
    );
    let main = mir
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("expected main function");
    assert!(main.blocks.iter().any(|block| {
        block.scope_depth > 0
            && matches!(block.terminator, Some(MirTerminator::Goto { .. }))
            && block
                .statements
                .iter()
                .any(|statement| matches!(statement, MirStatement::DropLocal { .. }))
    }));
    assert!(mir.verify().is_ok());
}

#[test]
fn lowers_class_drops_across_nested_branch_scopes() {
    let mir = lower(
        "class C { let value: Int32 = 1 }\n\
         fn main() {\n\
             loop {\n\
                 let outer = C()\n\
                 if true { let inner = C(); break } else { break }\n\
             }\n\
         }",
    );
    let main = mir
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("expected main function");
    let edge_drop_counts = main
        .blocks
        .iter()
        .filter(|block| block.scope_depth == 2)
        .map(|block| {
            block
                .statements
                .iter()
                .filter(|statement| matches!(statement, MirStatement::DropLocal { .. }))
                .count()
        })
        .filter(|count| *count > 0)
        .collect::<Vec<_>>();
    assert_eq!(edge_drop_counts.len(), 2);
    assert!(edge_drop_counts.contains(&1));
    assert!(edge_drop_counts.contains(&2));
    mir.verify().unwrap();
}

#[test]
fn lowers_function_scope_class_drop_after_branch_merge() {
    let mir = lower(
        "class C { let value: Int32 = 1 }\n\
         fn main() { let c = C(); let result = if true { 1 } else { 2 } }",
    );
    let main = mir
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("expected main function");
    assert!(main.blocks.iter().any(|block| {
        matches!(block.terminator, Some(MirTerminator::Return(_)))
            && block
                .statements
                .iter()
                .any(|statement| matches!(statement, MirStatement::DropLocal { .. }))
    }));
    mir.verify().unwrap();
}

#[test]
fn preserves_class_local_transferred_through_return() {
    let mir = lower(
        "class C { let value: Int32 = 1 }\n\
         fn make() -> C { let c = C(); c }\n\
         fn main() {}",
    );
    let make = mir
        .functions
        .iter()
        .find(|function| function.name == "make")
        .expect("expected make function");
    assert!(!make.blocks.iter().any(|block| block
        .statements
        .iter()
        .any(|statement| matches!(statement, MirStatement::DropLocal { .. }))));
    mir.verify().unwrap();
}

#[test]
fn moves_class_between_local_bindings() {
    let mir = lower(
        "class C { let value: Int32 = 1 }\n\
         fn main() { let first = C(); let second = first }",
    );
    let main = mir
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("expected main function");
    assert_eq!(
        main.blocks
            .iter()
            .flat_map(|block| &block.statements)
            .filter(|statement| matches!(statement, MirStatement::TakeLocal { .. }))
            .count(),
        1
    );
    assert_eq!(
        main.blocks
            .iter()
            .flat_map(|block| &block.statements)
            .filter(|statement| matches!(statement, MirStatement::DropLocal { .. }))
            .count(),
        1
    );
    mir.verify().unwrap();
}

#[test]
fn verifier_rejects_class_local_use_after_move() {
    let mir = lower(
        "class C { fn touch() {} }\n\
         fn main() { let first = C(); let second = first; first.touch() }",
    );
    let error = mir
        .verify()
        .expect_err("moved class local must be rejected");
    assert!(error.to_string().contains("after move"));
}

#[test]
fn class_method_calls_borrow_receiver_without_consuming_it() {
    let mir = lower(
        "class C { fn touch() {} }\n\
         fn main() { let value = C(); value.touch(); value.touch() }",
    );
    let main = mir
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("expected main function");
    assert_eq!(
        main.blocks
            .iter()
            .flat_map(|block| &block.statements)
            .filter(|statement| matches!(statement, MirStatement::BorrowLocal { .. }))
            .count(),
        2
    );
    assert!(!main
        .blocks
        .iter()
        .flat_map(|block| &block.statements)
        .any(|statement| matches!(statement, MirStatement::TakeLocal { .. })));
    mir.verify().unwrap();
}

#[test]
fn class_function_arguments_consume_owned_locals() {
    let mir = lower(
        "class C { fn touch() {} }\n\
         fn consume(value: C) {}\n\
         fn main() { let value = C(); consume(value); value.touch() }",
    );
    assert!(mir.verify().is_err());
}

#[test]
fn verifier_accepts_class_move_from_all_if_branches() {
    let mir = lower(
        "class C {}\n\
         fn choose(flag: Bool) -> C { let value = C(); if flag { value } else { value } }\n\
         fn main() {}",
    );
    mir.verify().unwrap();
}

#[test]
fn branch_local_debug_borrows_require_initialization_after_merge() {
    let mut mir = lower(
        "class C {} impl Debug for C { fn debug(&self) -> String { \"C\" } }\n\
         fn main() { let value = Some(C()); echo value; () }",
    );
    mir.verify().expect("a borrow used only in Some is valid");
    let main = mir.functions.iter_mut().find(|f| f.name == "main").unwrap();
    let local = main
        .locals
        .iter()
        .find(|local| local.name.starts_with("@debug/") && matches!(local.ty, Type::Class(_)))
        .unwrap();
    let destination = MirValueId(main.value_types.len());
    main.value_types.push(local.ty);
    main.value_ownership.push(MirOwnership::Borrowed);
    let statement = MirStatement::BorrowLocal {
        destination,
        local: local.id,
    };
    let merge = main
        .blocks
        .iter_mut()
        .find(|block| {
            block
                .statements
                .iter()
                .any(|statement| matches!(statement, MirStatement::Phi { .. }))
        })
        .unwrap();
    merge.statements.insert(1, statement);
    let error = mir
        .verify()
        .expect_err("Some's borrow is uninitialized on the None path");
    assert!(error.to_string().contains("after move"), "{error}");
}

#[test]
fn verifier_rejects_partial_class_move_across_if_merge() {
    let mir = lower(
        "class C {}\n\
         fn consume(value: C) {}\n\
         fn main() {\n\
             let value = C()\n\
             let path = if true { consume(value) }\n\
         }",
    );
    let error = mir
        .verify()
        .expect_err("partial class move must not cross a merge");
    assert!(error.to_string().contains("inconsistent local ownership"));
}

#[test]
fn aggregates_containing_classes_are_owned_and_moved() {
    let mir = lower(
        "class Token {}\n\
         struct Boxed { let token: Token }\n\
         enum MaybeToken { Some(token: Token); None }\n\
         fn consume_box(value: Boxed) {}\n\
         fn consume_tuple(value: (Token, Int32)) {}\n\
         fn consume_maybe(value: MaybeToken) {}\n\
         fn main() {\n\
             let boxed = Boxed(token: Token())\n\
             let tuple = (Token(), 1)\n\
             let maybe = MaybeToken.Some(token: Token())\n\
             consume_box(boxed)\n\
             consume_tuple(tuple)\n\
             consume_maybe(maybe)\n\
         }",
    );
    let main = mir
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("expected main function");
    for name in ["boxed", "tuple", "maybe"] {
        let local = main
            .locals
            .iter()
            .find(|local| local.name == name)
            .expect("expected aggregate local");
        assert_eq!(local.ownership, MirOwnership::Owned);
    }
    assert_eq!(
        main.blocks
            .iter()
            .flat_map(|block| &block.statements)
            .filter(|statement| matches!(statement, MirStatement::TakeLocal { .. }))
            .count(),
        3
    );
    mir.verify().unwrap();
}

#[test]
fn verifier_rejects_owned_aggregate_use_after_move() {
    let mir = lower(
        "class Token {}\n\
         struct Boxed { let token: Token }\n\
         fn consume(value: Boxed) {}\n\
         fn main() {\n\
             let value = Boxed(token: Token())\n\
             consume(value)\n\
             consume(value)\n\
         }",
    );
    let error = mir
        .verify()
        .expect_err("moved aggregate local must be rejected");
    assert!(error.to_string().contains("after move"));
}

#[test]
fn discarded_owned_aggregate_emits_drop() {
    let mir = lower(
        "class Token {}\n\
         struct Boxed { let token: Token }\n\
         fn main() { let value = Boxed(token: Token()); println(\"done\") }",
    );
    let main = &mir.functions[0];
    let local = main
        .locals
        .iter()
        .find(|local| local.name == "value")
        .expect("owned aggregate local");
    assert!(main.blocks.iter().any(|block| {
        block.statements.iter().any(|statement| {
            matches!(statement, MirStatement::DropLocal { local: dropped, .. } if *dropped == local.id)
        })
    }));
    mir.verify().unwrap();
}

#[test]
fn verifier_rejects_duplicate_owned_field_moves() {
    let (mut mir, index) = owned_pair_mir();
    let mut projections = 0;
    for statement in &mut mir.functions[index].blocks[0].statements {
        if let MirStatement::Project { access, .. } = statement {
            if projections == 1 {
                *access = MirFieldAccess::Index(0);
            }
            projections += 1;
        }
    }
    assert_eq!(projections, 2);

    let error = mir
        .verify()
        .expect_err("an owned field cannot be moved twice");
    assert!(error.to_string().contains("more than once"));
}

#[test]
fn verifier_rejects_deinit_with_unmoved_owned_fields() {
    let mut mir = lower(
        "class Token {}\n\
         fn take_first(pair: (Token, Token)) -> Token { pair.0 }\n\
         fn main() {}",
    );
    let index = mir
        .functions
        .iter()
        .position(|function| function.name == "take_first")
        .expect("expected take_first function");
    let removed = mir.functions[index].blocks[0]
        .statements
        .iter()
        .find_map(|statement| match statement {
            MirStatement::Project {
                destination,
                access: MirFieldAccess::Index(1),
                ..
            } => Some(*destination),
            _ => None,
        })
        .expect("expected second tuple field projection");
    mir.functions[index].blocks[0]
        .statements
        .retain(|statement| {
            !matches!(statement, MirStatement::Project { destination, .. } if *destination == removed)
                && !matches!(statement, MirStatement::Drop { value, .. } if *value == removed)
        });

    let error = mir
        .verify()
        .expect_err("deinit requires every owned field to be moved");
    assert!(error
        .to_string()
        .contains("before all owned fields are moved"));
}

#[test]
fn verifier_rejects_whole_aggregate_use_after_partial_move() {
    let (mut mir, index) = owned_pair_mir();
    let statements = &mut mir.functions[index].blocks[0].statements;
    let deinit = statements
        .iter()
        .position(|statement| matches!(statement, MirStatement::Deinit { .. }))
        .expect("expected aggregate deinit");
    let (destination, value) = match statements[deinit] {
        MirStatement::Deinit {
            destination, value, ..
        } => (destination, value),
        _ => unreachable!(),
    };
    statements[deinit] = MirStatement::Drop { destination, value };

    let error = mir
        .verify()
        .expect_err("the whole aggregate is unavailable after a field move");
    assert!(error.to_string().contains("uses partially moved aggregate"));
}

#[test]
fn verifier_rejects_forged_c_null_intrinsics() {
    let mir = lower("fn main() { let n = 1; let checked = CStr.null().is_null() }");
    mir.verify().unwrap();
    for case in [
        "integer null",
        "wrong destination",
        "extra null argument",
        "missing receiver",
        "wrong receiver",
        "wrong parameter",
    ] {
        let mut forged = lower("fn main() { let n = 1; let checked = CStr.null().is_null() }");
        let function = &mut forged.functions[0];
        for statement in function.blocks.iter_mut().flat_map(|b| &mut b.statements) {
            if let MirStatement::RuntimeCall {
                destination,
                intrinsic,
                arguments,
            } = statement
            {
                match (&*intrinsic, case) {
                    (RuntimeIntrinsic::CPointerNull(_), "integer null") => {
                        *intrinsic = RuntimeIntrinsic::CPointerNull(Type::I32);
                        function.value_types[destination.0] = Type::I32;
                    }
                    (RuntimeIntrinsic::CPointerNull(_), "wrong destination") => {
                        function.value_types[destination.0] = Type::I32;
                    }
                    (RuntimeIntrinsic::CPointerNull(_), "extra null argument") => {
                        arguments.push(MirCallArgument {
                            parameter: 0,
                            value: *destination,
                        });
                    }
                    (RuntimeIntrinsic::CPointerIsNull, "missing receiver") => arguments.clear(),
                    (RuntimeIntrinsic::CPointerIsNull, "wrong receiver") => {
                        arguments[0].value = MirValueId(0);
                    }
                    (RuntimeIntrinsic::CPointerIsNull, "wrong parameter") => {
                        arguments[0].parameter = 1
                    }
                    _ => {}
                }
            }
        }
        assert!(forged.verify().is_err(), "accepted {case}");
    }
}

#[test]
fn verifier_rejects_forged_ordering_intrinsics() {
    let source = "fn main() { let compared = PartialOrd.partial_compare(1.0, 2.0) }";
    lower(source).verify().unwrap();
    for case in [
        "float Ord",
        "wrong type",
        "wrong destination",
        "missing operand",
        "wrong parameter",
        "borrowed operand",
    ] {
        let mut forged = lower(source);
        let function = &mut forged.functions[0];
        let mut changed = false;
        for statement in function.blocks.iter_mut().flat_map(|b| &mut b.statements) {
            if let MirStatement::RuntimeCall {
                destination,
                intrinsic,
                arguments,
            } = statement
            {
                if matches!(intrinsic, RuntimeIntrinsic::PartialCompare(_)) {
                    match case {
                        "float Ord" => *intrinsic = RuntimeIntrinsic::Compare(Type::F64),
                        "wrong type" => *intrinsic = RuntimeIntrinsic::PartialCompare(Type::Bytes),
                        "wrong destination" => function.value_types[destination.0] = Type::Bool,
                        "missing operand" => {
                            arguments.pop();
                        }
                        "wrong parameter" => arguments[0].parameter = 1,
                        "borrowed operand" => {
                            function.value_ownership[arguments[0].value.0] = MirOwnership::Borrowed
                        }
                        _ => unreachable!(),
                    }
                    changed = true;
                }
            }
        }
        assert!(changed);
        assert!(forged.verify().is_err(), "accepted {case}");
    }
}
