use super::*;

#[test]
fn verifier_rejects_dropping_a_capability_without_releasing_its_abort_lease() {
    let mut mir = lower(
        r#"
        eff Failure { @aborts fn stop() -> Unit }
        class State { let value: Int32 }
        fn main() effects { Failure } {
            let counter = Cown.new(State(value: 0))
            when (counter) |state| { Failure.stop() }
        }
        "#,
    );
    mir.verify()
        .expect("abort cleanup must verify before mutation");
    let release = mir
        .functions
        .iter_mut()
        .flat_map(|function| &mut function.blocks)
        .flat_map(|block| &mut block.statements)
        .find(|statement| {
            matches!(
                statement,
                MirStatement::RuntimeCall {
                    intrinsic: RuntimeIntrinsic::CownRelease,
                    ..
                }
            )
        })
        .expect("abort edge must release the lease");
    let MirStatement::RuntimeCall {
        destination,
        arguments,
        ..
    } = release
    else {
        unreachable!()
    };
    *release = MirStatement::Drop {
        destination: *destination,
        value: arguments[0].value,
    };
    let error = mir.verify().unwrap_err().to_string();
    assert!(error.contains("holding a Cown lease"), "{error}");
}

#[test]
fn verifier_rejects_non_subset_dynamic_upcast() {
    let mut mir = lower(
        "trait A { fn a(&self) -> Unit }\n\
         trait B { fn b(&self) -> Unit }\n\
         trait C { fn c(&self) -> Unit }\n\
         fn narrow(value: Dyn(A + B), other: Dyn(C)) -> Dyn(A) { Dyn(A)(value) }\n\
         fn main() {}",
    );
    let index = mir
        .functions
        .iter()
        .position(|function| function.name == "narrow")
        .unwrap();
    let function = &mut mir.functions[index];
    let unrelated = function.parameters[1].ty;
    let destination = function
        .blocks
        .iter()
        .flat_map(|block| &block.statements)
        .find_map(|statement| {
            if let MirStatement::DynamicUpcast { destination, .. } = statement {
                Some(*destination)
            } else {
                None
            }
        })
        .unwrap();
    function.value_types[destination.0] = unrelated;
    let error = verify_one(&mir, index).expect_err("upcasts may only remove traits");
    assert!(
        error
            .to_string()
            .contains("source interface to include trait 'C'"),
        "{error}"
    );
}

#[test]
fn verifier_rejects_moving_fields_out_of_classes() {
    let mut mir = lower(
        "class Token {}\n\
         class Owner { let token: Token }\n\
         fn identity(owner: Owner) -> Owner { owner }\n\
         fn main() {}",
    );
    let index = mir
        .functions
        .iter()
        .position(|function| function.name == "identity")
        .expect("expected identity function");
    let base = mir.functions[index].blocks[0]
        .statements
        .iter()
        .find_map(|statement| match statement {
            MirStatement::TakeLocal { destination, .. } => Some(*destination),
            _ => None,
        })
        .expect("expected owned class value");
    let destination = MirValueId(mir.functions[index].value_types.len());
    mir.functions[index].value_types.push(Type::Class(0));
    mir.functions[index]
        .value_ownership
        .push(MirOwnership::Owned);
    mir.functions[index].blocks[0]
        .statements
        .push(MirStatement::Project {
            destination,
            base,
            access: MirFieldAccess::Index(0),
        });

    let error = mir
        .verify()
        .expect_err("class fields cannot be moved out independently");
    assert!(error.to_string().contains("moves a field out of a class"));
}

#[test]
fn verifier_rejects_partial_moves_across_basic_blocks() {
    let (mut mir, index) = owned_pair_mir();
    let function = &mut mir.functions[index];
    let deinit = function.blocks[0]
        .statements
        .iter()
        .position(|statement| matches!(statement, MirStatement::Deinit { .. }))
        .expect("expected aggregate deinit");
    let deinit = function.blocks[0].statements.remove(deinit);
    let terminator = function.blocks[0]
        .terminator
        .take()
        .expect("expected return terminator");
    let next = MirBlockId(function.blocks.len());
    function.blocks[0].terminator = Some(MirTerminator::Goto {
        target: next,
        arguments: Vec::new(),
    });
    function.blocks.push(MirBlock {
        id: next,
        scoped: false,
        scope_depth: 0,
        statements: vec![deinit],
        terminator: Some(terminator),
    });

    let error = mir
        .verify()
        .expect_err("partial moves must finish in their basic block");
    assert!(error.to_string().contains("across a basic block edge"));
}

#[test]
fn verifier_rejects_mixed_enum_variant_deinit() {
    let mut mir = lower(
        "class Token {}\n\
         enum Choice { Full(left: Token, right: Token); Empty }\n\
         fn take(value: Choice) -> Token {\n\
             match value {\n\
                 Choice.Full(left, right: _) => left\n\
                 Choice.Empty => Token()\n\
             }\n\
         }\n\
         fn main() {}",
    );
    let index = mir
        .functions
        .iter()
        .position(|function| function.name == "take")
        .expect("expected take function");
    let variant = mir.functions[index]
        .blocks
        .iter_mut()
        .flat_map(|block| &mut block.statements)
        .find_map(|statement| match statement {
            MirStatement::Deinit {
                variant: variant @ Some(0),
                ..
            } => Some(variant),
            _ => None,
        })
        .expect("expected full variant deinit");
    *variant = Some(1);

    let error = mir
        .verify()
        .expect_err("enum fields must match the deinitialized variant");
    assert!(error.to_string().contains("mixes enum variants"));
}

#[test]
fn verifier_rejects_ownership_ops_on_copy_values() {
    let mir = lower("fn main() { 1 }");
    let source = match mir.functions[0].blocks[0].statements[0] {
        MirStatement::Const { destination, .. } => destination,
        _ => panic!("expected integer constant"),
    };
    for operation in ["dup", "move", "drop"] {
        let mut candidate = lower("fn main() { 1 }");
        let source = match candidate.functions[0].blocks[0].statements[0] {
            MirStatement::Const { destination, .. } => destination,
            _ => unreachable!(),
        };
        let destination = MirValueId(candidate.functions[0].value_types.len());
        candidate.functions[0]
            .value_types
            .push(if operation == "drop" {
                Type::Unit
            } else {
                Type::I32
            });
        candidate.functions[0]
            .value_ownership
            .push(MirOwnership::Copy);
        let statement = match operation {
            "dup" => MirStatement::Dup {
                destination,
                value: source,
            },
            "move" => MirStatement::Move {
                destination,
                value: source,
            },
            _ => MirStatement::Drop {
                destination,
                value: source,
            },
        };
        candidate.functions[0].blocks[0].statements.push(statement);
        assert!(
            candidate.verify().is_err(),
            "{operation} should reject copy values"
        );
    }
    assert_eq!(
        mir.functions[0].value_ownership[source.0],
        MirOwnership::Copy
    );
}

#[test]
fn verifier_rejects_use_after_move() {
    let mut mir = lower("class C { let value: Int32 = 1 } fn main() { let c = C() }");
    let source = mir.functions[0]
        .blocks
        .iter()
        .flat_map(|block| block.statements.iter())
        .find_map(|statement| match statement {
            MirStatement::Construct {
                destination,
                type_id: MirTypeId::Class(_),
                ..
            } => Some(*destination),
            _ => None,
        })
        .expect("expected class construction");
    let moved = MirValueId(mir.functions[0].value_types.len());
    let source_type = mir.functions[0].value_types[source.0];
    mir.functions[0].value_types.push(source_type);
    mir.functions[0].value_ownership.push(MirOwnership::Owned);
    let unit = MirValueId(mir.functions[0].value_types.len());
    mir.functions[0].value_types.push(Type::Unit);
    mir.functions[0].value_ownership.push(MirOwnership::Copy);
    let statements = &mut mir.functions[0].blocks[0].statements;
    statements.push(MirStatement::Move {
        destination: moved,
        value: source,
    });
    statements.push(MirStatement::Drop {
        destination: unit,
        value: source,
    });
    assert!(mir.verify().is_err());
}

#[test]
fn verifier_rejects_invalid_ids_before_dense_set_insertion() {
    let mut mir = lower("fn main() {}");
    let function = mir.functions.iter_mut().find(|f| f.name == "main").unwrap();
    function.blocks[function.entry.0]
        .statements
        .push(MirStatement::Unit {
            destination: MirValueId(usize::MAX),
        });
    assert!(mir.verify().is_err());

    let mut mir = lower("class Token {}\nfn main() { let token = Token() }");
    let function = mir.functions.iter_mut().find(|f| f.name == "main").unwrap();
    function.locals[0].id = MirLocalId(usize::MAX);
    let error = mir
        .verify()
        .expect_err("invalid local metadata must not index a dense set");
    assert!(error.to_string().contains("invalid local id"), "{error}");
}
