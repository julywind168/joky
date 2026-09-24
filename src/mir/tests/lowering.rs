use super::*;

#[test]
fn capture_free_suspending_closure_uses_direct_continuation() {
    let source = "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
                  fn main() effects { time } { let f = fn () -> Unit { time.sleep(1ms) }; f() }";
    let program = crate::syntax::parse_program(source).expect("source should parse");
    let types = crate::sema::check_program(&program).expect("source should type check");
    let core = crate::hir::CoreProgram::lower(program, types).expect("HIR should lower");
    let mir = MirProgram::lower(&core).expect("suspending indirect call should lower");
    let statement = mir.functions()[0]
        .blocks
        .iter()
        .flat_map(|block| block.statements.iter())
        .find_map(|statement| match statement {
            MirStatement::Call { continuation, .. } => Some(*continuation),
            _ => None,
        })
        .expect("indirect call should remain in MIR");
    assert!(statement.is_some());
}

#[test]
fn monomorphizes_generic_list_functions_before_mir() {
    let mir = lower(
        "fn reverse(T: type, value: List(T)) -> List(T) { value.reverse() }\n\
         fn main() { let reversed = reverse(value: List(1, 2, 3)); let first = reversed.head() }",
    );
    let function = mir
        .functions()
        .iter()
        .find(|function| function.name == "reverse$$Int32")
        .expect("generic List function should have a concrete MIR instance");
    assert_eq!(function.parameters[0].ty, function.return_type);
    assert!(matches!(function.return_type, Type::List(_)));
}

#[test]
fn verifier_rejects_owned_effect_argument_with_borrowed_ownership() {
    let mut mir = lower(
        "class Token {}\n\
         eff Build { fn make(token: Token) -> Int32 }\n\
         fn ask(token: Token) -> Int32 effects { Build } { Build.make(token: token) }\n\
         fn main() -> Int32 { do { ask(Token()) + 1 } with { Build.make(token) => 41 } }",
    );
    let index = mir
        .functions
        .iter()
        .position(|function| {
            function
                .blocks
                .iter()
                .flat_map(|block| &block.statements)
                .any(|statement| matches!(statement, MirStatement::HandlerRequest { .. }))
        })
        .expect("handler request function");
    let function = &mut mir.functions[index];
    let argument = function
        .blocks
        .iter()
        .flat_map(|block| &block.statements)
        .find_map(|statement| match statement {
            MirStatement::HandlerRequest { arguments, .. } => {
                arguments.first().map(|argument| argument.value)
            }
            _ => None,
        })
        .expect("handler request argument");
    function.value_ownership[argument.0] = MirOwnership::Borrowed;
    assert!(verify_one(&mir, index).is_err());
}

#[test]
fn verifier_rejects_owned_value_for_shared_effect_argument() {
    let mut mir = lower(
        "eff Console { fn read(prompt: String) -> Int32 }\n\
         fn ask() -> Int32 effects { Console } { Console.read(prompt: \"age\") }\n\
         fn main() -> Int32 { do { ask() + 1 } with { Console.read(prompt) => 41 } }",
    );
    let index = mir
        .functions
        .iter()
        .position(|function| {
            function
                .blocks
                .iter()
                .flat_map(|block| &block.statements)
                .any(|statement| matches!(statement, MirStatement::HandlerRequest { .. }))
        })
        .expect("handler request function");
    let function = &mut mir.functions[index];
    let argument = function
        .blocks
        .iter()
        .flat_map(|block| &block.statements)
        .find_map(|statement| match statement {
            MirStatement::HandlerRequest { arguments, .. } => {
                arguments.first().map(|argument| argument.value)
            }
            _ => None,
        })
        .expect("handler request argument");
    function.value_ownership[argument.0] = MirOwnership::Owned;
    assert!(verify_one(&mir, index).is_err());
}
#[test]
fn lowers_unhandled_normal_effect_to_runtime_request() {
    let mir = lower(
        "eff Clock { fn now() -> Int64 }\n\
         fn main() -> Int64 effects { Clock } { Clock.now() }",
    );
    assert!(mir.functions()[0].blocks.iter().any(|block| {
        block.statements.iter().any(|statement| {
            matches!(statement, MirStatement::HandlerRequest { operation, .. } if operation.operation == 0)
        })
    }));
    assert!(!mir.functions()[0].blocks.iter().any(|block| {
        block.statements.iter().any(|statement| {
            matches!(statement, MirStatement::TaskAbort { operation, .. } if operation.operation == 0)
        })
    }));
    mir.verify()
        .expect("unhandled normal request MIR should verify");
}

#[test]
fn lowers_single_cown_when_to_runtime_lease_operations() {
    let mir = lower(
        "class Counter { var value: Int32 = 0\n fn increment() { self.value = self.value + 1 } }\n\
         fn main() { let counter = Cown.new(Counter(value: 0)); let updated = when (counter) |state| { state.increment(); state.value }; println(updated) }",
    );
    let main = mir
        .functions()
        .iter()
        .find(|function| function.name == "main")
        .expect("main MIR function");
    let intrinsics = main
        .blocks
        .iter()
        .flat_map(|block| block.statements.iter())
        .filter_map(|statement| match statement {
            MirStatement::RuntimeCall { intrinsic, .. } => Some(*intrinsic),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(intrinsics
        .iter()
        .any(|intrinsic| matches!(intrinsic, RuntimeIntrinsic::CownNew(Type::Class(_)))));
    assert!(main
        .blocks
        .iter()
        .flat_map(|block| &block.statements)
        .any(|statement| matches!(statement, MirStatement::CownAcquire { .. })));
    assert!(intrinsics
        .iter()
        .any(|intrinsic| matches!(intrinsic, RuntimeIntrinsic::CownPayload(Type::Class(_)))));
    assert!(intrinsics
        .iter()
        .any(|intrinsic| matches!(intrinsic, RuntimeIntrinsic::CownRelease)));
    mir.verify().expect("single-Cown MIR should verify");
}

#[test]
fn lowers_unhandled_aborting_task_effect_to_task_abort() {
    let mir = lower(
        "eff Failure { @aborts fn stop() -> Unit }\n\
         fn main() effects { Failure } { let ignored = parallel {\n| Failure.stop()\n| loop { }\n} }",
    );
    assert!(mir.functions().iter().any(|function| {
        function.is_task
            && function.blocks.iter().any(|block| {
                block
                    .statements
                    .iter()
                    .any(|statement| matches!(statement, MirStatement::TaskAbort { .. }))
            })
    }));
    mir.verify().expect("aborting task MIR should verify");
}

#[test]
fn lowers_child_abort_payload_and_parent_handler_dispatch() {
    let mir = lower(
        "eff Failure { @aborts fn stop(code: Int32, message: String) -> Unit }\n\
         fn fail() effects { Failure } { Failure.stop(code: 7, message: \"bad\") }\n\
         fn main() -> Int32 { do { let ignored = parallel {\n| fail()\n| loop { }\n}; 0 } with { Failure.stop(code, message) => code } }",
    );
    let main = mir
        .functions()
        .iter()
        .find(|function| function.name == "main")
        .expect("main MIR function");
    assert!(main.blocks.iter().any(|block| block
        .statements
        .iter()
        .any(|statement| matches!(statement, MirStatement::TaskFailureOperation { .. }))));
    assert_eq!(
        main.blocks
            .iter()
            .flat_map(|block| &block.statements)
            .filter(|statement| matches!(statement, MirStatement::TaskFailurePayload { .. }))
            .count(),
        2
    );
    mir.verify()
        .expect("typed child abort dispatch MIR should verify");
}

#[test]
fn verifier_rejects_invalid_task_failure_payload_parameter() {
    let mut mir = lower(
        "eff Failure { @aborts fn stop(code: Int32) -> Unit }\n\
         fn fail() effects { Failure } { Failure.stop(code: 7) }\n\
         fn main() -> Int32 { do { let ignored = parallel {\n| fail()\n| loop { }\n}; 0 } with { Failure.stop(code) => code } }",
    );
    let payload = mir
        .functions
        .iter_mut()
        .flat_map(|function| &mut function.blocks)
        .flat_map(|block| &mut block.statements)
        .find_map(|statement| match statement {
            MirStatement::TaskFailurePayload { parameter, .. } => Some(parameter),
            _ => None,
        })
        .expect("task failure payload statement");
    *payload = usize::MAX;
    assert!(mir.verify().is_err());
}

#[test]
fn lowers_unhandled_nested_failure_to_abort_rethrow() {
    let mir = lower(
        "eff Failure { @aborts fn stop() -> Unit }\n\
         fn fail() effects { Failure } { Failure.stop() }\n\
         fn main() effects { Failure } { let ignored = parallel {\n| fail()\n| loop { }\n} }",
    );
    assert!(mir.functions().iter().any(|function| {
        function
            .blocks
            .iter()
            .flat_map(|block| &block.statements)
            .any(|statement| matches!(statement, MirStatement::TaskFailureRethrow { .. }))
    }));
    mir.verify()
        .expect("unhandled nested failure should rethrow through Abort");
}

#[test]
fn verifier_rejects_task_failure_claim_without_immediate_scope_exit() {
    let mut mir = lower(
        "eff Failure { @aborts fn stop(code: Int32) -> Unit }\n\
         fn fail() effects { Failure } { Failure.stop(code: 7) }\n\
         fn main() -> Int32 { do { let ignored = parallel {\n| fail()\n| loop { }\n}; 0 } with { Failure.stop(code) => code } }",
    );
    let function = mir
        .functions
        .iter_mut()
        .find(|function| function.name == "main")
        .expect("main MIR function");
    let block = function
        .blocks
        .iter_mut()
        .find(|block| {
            block
                .statements
                .iter()
                .any(|statement| matches!(statement, MirStatement::TaskFailureClaim { .. }))
        })
        .expect("failure claim block");
    let claim = block
        .statements
        .iter()
        .position(|statement| matches!(statement, MirStatement::TaskFailureClaim { .. }))
        .expect("failure claim");
    block.statements.remove(claim + 1);
    mir.verify()
        .expect_err("claim must be followed immediately by scope cleanup");
}

#[test]
fn verifier_rejects_statements_after_task_failure_rethrow() {
    let mut mir = lower(
        "eff Failure { @aborts fn stop() -> Unit }\n\
         fn fail() effects { Failure } { Failure.stop() }\n\
         fn main() effects { Failure } { let ignored = parallel {\n| fail()\n| loop { }\n} }",
    );
    let function = mir
        .functions
        .iter_mut()
        .find(|function| function.name == "main")
        .expect("main MIR function");
    let block = function
        .blocks
        .iter_mut()
        .find(|block| {
            block
                .statements
                .iter()
                .any(|statement| matches!(statement, MirStatement::TaskFailureRethrow { .. }))
        })
        .expect("rethrow block");
    block.statements.push(MirStatement::HandlerExit);
    mir.verify()
        .expect_err("rethrow must not continue with ordinary statements");
}

#[test]
fn lowers_parallel_to_structured_task_mir() {
    let program = syntax::parse_program(
        "fn first() -> Int32 { 1 }\n\
         fn second() -> Int32 { 2 }\n\
         fn main() { let values = parallel {\n| first()\n| second()\n} }",
    )
    .expect("program should parse");
    let types = sema::check_program(&program).expect("program should type check");
    let core = CoreProgram::lower(program, types).expect("program should lower to HIR");
    let mir = MirProgram::lower(&core).expect("MIR must preserve concurrent task structure");
    let main = mir
        .functions()
        .iter()
        .find(|function| function.name == "main")
        .expect("main function should exist");
    let statements = &main.blocks[0].statements;
    assert!(statements
        .iter()
        .any(|statement| matches!(statement, MirStatement::ScopeEnter { .. })));
    assert_eq!(
        statements
            .iter()
            .filter(|statement| matches!(statement, MirStatement::TaskCreate { .. }))
            .count(),
        2
    );
    assert_eq!(
        main.blocks
            .iter()
            .flat_map(|block| &block.statements)
            .filter(|statement| matches!(statement, MirStatement::TaskJoin { .. }))
            .count(),
        2
    );
    assert_eq!(
        main.blocks
            .iter()
            .flat_map(|block| &block.statements)
            .filter(|statement| matches!(statement, MirStatement::TaskClaimResult { .. }))
            .count(),
        2
    );
    mir.verify().expect("structured task MIR should verify");
}

#[test]
fn lowers_captured_values_into_task_function_parameters() {
    let mir = lower(
        "fn main() {\n\
         let offset = 40\n\
         let values = parallel {\n| offset + 1\n| offset + 2\n}\n\
         }",
    );
    let tasks = mir
        .functions()
        .iter()
        .filter(|function| function.name.starts_with("__task_main_"))
        .collect::<Vec<_>>();
    assert_eq!(tasks.len(), 2);
    assert!(tasks
        .iter()
        .all(|function| function.parameters.len() == 1 && function.parameters[0].name == "offset"));
    mir.verify()
        .expect("task captures should be passed through typed parameters");
}

#[test]
fn lowers_race_to_start_and_select() {
    let mir = lower(
        "fn first() -> Int32 { 1 }\n\
         fn second() -> Int32 { 2 }\n\
         fn main() { let value = race {\n| first()\n| second()\n} }",
    );
    let main = mir
        .functions()
        .iter()
        .find(|function| function.name == "main")
        .expect("main function should exist");
    assert!(main.blocks[0]
        .statements
        .iter()
        .any(|statement| matches!(statement, MirStatement::RaceStart { .. })));
    assert!(main
        .blocks
        .iter()
        .flat_map(|block| &block.statements)
        .any(|statement| matches!(statement, MirStatement::RaceSelect { .. })));
    mir.verify().expect("race MIR should verify");
}

#[test]
fn lowers_branch_into_the_implicit_root_scope() {
    let mir = lower("fn work() -> Int32 { 1 } fn main() { branch { work() } }");
    let main = mir
        .functions()
        .iter()
        .find(|function| function.name == "main")
        .expect("main function should exist");
    assert!(main.blocks[0].statements.iter().any(|statement| {
        matches!(
            statement,
            MirStatement::TaskCreate {
                scope: MirScopeId(0),
                ..
            }
        )
    }));
    assert!(main
        .blocks
        .iter()
        .flat_map(|block| &block.statements)
        .any(|statement| matches!(
            statement,
            MirStatement::ScopeExit {
                scope: MirScopeId(0)
            }
        )));
    mir.verify().expect("root task scope should verify");
}

#[test]
fn lowers_nested_parallel_into_nested_task_functions() {
    let mir = lower(
        "fn main() {\n\
         let offset = 40\n\
         let values = parallel {\n| parallel {\n| offset + 1\n| offset + 2\n}\n| offset + 3\n}\n\
         }",
    );
    assert_eq!(
        mir.functions()
            .iter()
            .filter(|function| function.name.starts_with("__task_"))
            .count(),
        4
    );
    mir.verify()
        .expect("nested task functions should preserve their capture boundary");
}

#[test]
fn verifier_rejects_join_of_unknown_task() {
    let mut mir = lower("fn main() { let value = 1 }");
    let function = &mut mir.functions[0];
    let destination = function.value_types.len();
    function.value_types.push(Type::I32);
    function.value_ownership.push(MirOwnership::Copy);
    function.blocks[0].statements.insert(
        1,
        MirStatement::TaskJoin {
            destination: MirValueId(destination),
            scope: MirScopeId(0),
            task: MirTaskId(99),
        },
    );
    let error = verify_one(&mir, 0).expect_err("unknown task must be rejected");
    assert!(error.to_string().contains("joins unknown task"));
}

#[test]
fn lowering_rejects_borrowed_receiver_capture_in_task() {
    let program = syntax::parse_program(
        "class Counter { fn start() { branch { self } } } fn main() { let counter = Counter(); counter.start() }",
    )
    .expect("program should parse");
    let types = sema::check_program(&program).expect("program should type check");
    let core = CoreProgram::lower(program, types).expect("program should lower to HIR");
    let error = MirProgram::lower(&core).expect_err("task cannot capture borrowed receiver");
    assert!(error
        .to_string()
        .contains("cannot capture borrowed local 'self' into task"));
}

#[test]
fn verifier_rejects_borrowed_task_capture() {
    let mut mir =
        lower("class Token {} fn take(value: Token) {} fn main() { let token = Token() }");
    let main_index = mir
        .functions
        .iter()
        .position(|function| function.name == "main")
        .expect("main function should exist");
    let take = mir
        .functions
        .iter()
        .find(|function| function.name == "take")
        .expect("take function should exist")
        .id;
    let main = &mut mir.functions[main_index];
    let token = main
        .locals
        .iter()
        .find(|local| local.name == "token")
        .expect("token local should exist")
        .id;
    let borrowed = MirValueId(main.value_types.len());
    main.value_types.push(main.locals[token.0].ty);
    main.value_ownership.push(MirOwnership::Borrowed);
    let insert_at = main.blocks[0]
        .statements
        .iter()
        .position(|statement| matches!(statement, MirStatement::ScopeExit { .. }))
        .expect("root scope should exit before return");
    main.blocks[0].statements.splice(
        insert_at..insert_at,
        [
            MirStatement::BorrowLocal {
                destination: borrowed,
                local: token,
            },
            MirStatement::TaskCreate {
                scope: MirScopeId(0),
                task: MirTaskId(0),
                function: take,
                arguments: vec![MirCallArgument {
                    parameter: 0,
                    value: borrowed,
                }],
            },
        ],
    );
    let error = verify_one(&mir, main_index).expect_err("borrowed task capture must be rejected");
    assert!(error.to_string().contains("captures borrowed value"));
}

#[test]
fn verifier_rejects_moving_one_class_into_multiple_tasks() {
    let mir = lower(
        "class Token {}\n\
         fn main() {\n\
         let token = Token()\n\
         let values = parallel {\n| token\n| token\n}\n\
         }",
    );
    let error = mir
        .verify()
        .expect_err("a class can be moved into only one task");
    assert!(error.to_string().contains("after move"));
}

#[test]
fn verifier_rejects_borrowed_closure_capture() {
    let mut mir = lower(
        "fn main() { let value = 1; let read = fn () -> Int32 { value }; let ignored = read() }",
    );
    let main_index = mir
        .functions
        .iter()
        .position(|function| function.name == "main")
        .expect("main MIR function");
    let function = &mut mir.functions[main_index];
    let capture = function
        .blocks
        .iter()
        .flat_map(|block| &block.statements)
        .find_map(|statement| match statement {
            MirStatement::FunctionValue { captures, .. } => captures.first().copied(),
            _ => None,
        })
        .expect("closure should capture the outer value");
    function.value_ownership[capture.0] = MirOwnership::Borrowed;

    let error =
        verify_one(&mir, main_index).expect_err("borrowed closure capture must be rejected");
    assert!(
        error
            .to_string()
            .contains("captures a borrowed value in an escaping closure"),
        "unexpected verifier error: {error}"
    );
}

#[test]
fn lowering_rejects_closure_capture_of_borrowed_self() {
    let program = syntax::parse_program(
        "class Counter {
         let value: Int32 = 1
         fn callback() -> fn() -> Int32 { move fn () -> Int32 { self.value } }
         }
         fn main() {}",
    )
    .expect("program should parse");
    let types = sema::check_program(&program).expect("program should type check");
    let core = CoreProgram::lower(program, types).expect("program should lower to HIR");
    let mir = MirProgram::lower(&core).expect("MIR lowering should preserve the borrow");
    let error = mir
        .verify()
        .expect_err("borrowed self cannot escape into a closure");
    assert!(error
        .to_string()
        .contains("captures a borrowed value in an escaping closure"));
}

#[test]
fn verifier_accepts_move_closure_capture_of_owned_value() {
    let mir = lower(
        "class Counter { let value: Int32 = 1 }
         fn main() {
         let counter = Counter(value: 1);
         let read = move fn () -> Int32 { counter.value };
         println(read())
         }",
    );
    mir.verify()
        .expect("move closure should own its captured class value");
}

#[test]
fn lowers_direct_effect_handler_to_cfg_join() {
    let mir = lower(
        "eff Clock { fn now() -> Int64 }\n\
         fn main() -> Int64 { let value = do { Clock.now() } with { Clock.now() => 42 }; value }",
    );
    assert!(mir.functions()[0].blocks.iter().any(|block| {
        block
            .statements
            .iter()
            .any(|statement| matches!(statement, MirStatement::Phi { .. }))
    }));
    assert!(!mir.functions()[0].blocks.iter().any(|block| {
        block
            .statements
            .iter()
            .any(|statement| matches!(statement, MirStatement::TaskCreate { .. }))
    }));
    mir.verify().expect("normal handler CFG should verify");
}

#[test]
fn lowers_effect_handler_parameter_into_target_local() {
    let mir = lower(
        "eff Console { fn read(prompt: String) -> Int64 }\n\
         fn main() -> Int64 { let value = do { Console.read(prompt: \"age\") } with { Console.read(prompt) => 42 }; value }",
    );
    let function = &mir.functions()[0];
    assert!(function.blocks.iter().any(|block| {
        block.statements.iter().any(|statement| {
            matches!(statement, MirStatement::HandlerEnter { handlers } if handlers.len() == 1)
        })
    }));
    assert!(function.blocks.iter().any(|block| {
        block
            .statements
            .iter()
            .any(|statement| matches!(statement, MirStatement::Bind { .. }))
    }));
    assert!(!function.blocks.iter().any(|block| {
        block
            .statements
            .iter()
            .any(|statement| matches!(statement, MirStatement::TaskCreate { .. }))
    }));
    mir.verify().expect("handler parameter CFG should verify");
}

#[test]
fn lowers_dynamic_normal_handler_request_with_continuation() {
    let mir = lower(
        "eff Console { fn read(prompt: String) -> Int64 }\n\
         fn ask() -> Int64 effects { Console } { Console.read(prompt: \"age\") }\n\
         fn main() -> Int64 {\n\
             do { ask() + 1 } with { Console.read(prompt) => 41 }\n\
         }",
    );
    assert!(mir.functions().iter().any(|function| function
        .blocks
        .iter()
        .flat_map(|block| &block.statements)
        .any(|statement| matches!(statement, MirStatement::HandlerRequest { .. }))));
    assert!(!mir.functions().iter().any(|function| function
        .blocks
        .iter()
        .flat_map(|block| &block.statements)
        .any(|statement| matches!(
            statement,
            MirStatement::TaskCreate { .. } | MirStatement::TaskAbort { .. }
        ))));
    assert!(mir.functions().iter().any(|function| {
        function
            .continuations
            .iter()
            .any(|continuation| continuation.kind == MirContinuationKind::Normal)
    }));
    mir.verify()
        .expect("dynamic normal handler request should verify");
}

#[test]
fn lowers_complex_direct_normal_handler_without_a_private_task() {
    let mir = lower(
        "eff Console { fn read(prompt: String) -> Int64 }\n\
         fn main() -> Int64 {\n\
             do { let answer = Console.read(prompt: \"age\")\n\
                  answer + 1 }\
             with { Console.read(prompt) => 41 }\n\
         }",
    );
    let function = mir
        .functions()
        .iter()
        .find(|function| function.name == "main")
        .expect("main MIR function");
    assert!(function
        .blocks
        .iter()
        .flat_map(|block| &block.statements)
        .any(|statement| matches!(statement, MirStatement::HandlerRequest { .. })));
    assert!(!function
        .blocks
        .iter()
        .flat_map(|block| &block.statements)
        .any(|statement| matches!(statement, MirStatement::TaskCreate { .. })));
    mir.verify()
        .expect("complex direct normal handler should verify");
}

#[test]
fn lowers_resumable_handler_as_a_direct_continuation() {
    let mir = lower(
        "eff Ask { @resumable fn question(prompt: String) -> String }\n\
         fn main() -> String {\n\
             do { let value = Ask.question(\"Joky\"); value.concat(\"!\") }\n\
                 with { Ask.question(prompt) => resume(prompt) }\n\
         }",
    );
    let function = &mir.functions()[0];
    assert!(!function
        .blocks
        .iter()
        .flat_map(|block| &block.statements)
        .any(|statement| matches!(
            statement,
            MirStatement::TaskCreate { .. } | MirStatement::TaskAbort { .. }
        )));
    assert!(function
        .blocks
        .iter()
        .flat_map(|block| &block.statements)
        .any(|statement| matches!(statement, MirStatement::Bind { .. })));
    mir.verify()
        .expect("resumable handler continuation should verify");
}

#[test]
fn lowers_parameterized_constant_resumable_handler_to_runtime_frame() {
    let mir = lower(
        "eff Ask { @resumable fn question(prompt: Int32) -> Int32 }\n\
         fn main() -> Int32 {\n\
             do { Ask.question(41) } with { Ask.question(prompt) => resume(prompt) }\n\
         }",
    );
    let function = mir
        .functions()
        .iter()
        .find(|function| function.name == "main")
        .expect("main MIR function");
    let handler = function
        .blocks
        .iter()
        .flat_map(|block| &block.statements)
        .find_map(|statement| match statement {
            MirStatement::HandlerEnter { handlers } => handlers.first(),
            _ => None,
        })
        .expect("runtime handler frame");
    assert_eq!(handler.resumable_parameter, Some(0));
    mir.verify()
        .expect("parameterized constant resumable handler should verify");
}

#[test]
fn lowers_capture_free_resumable_handler_body_to_a_synthetic_function() {
    let mir = lower(
        "eff Calc { @resumable fn adjust(value: Int64) -> Int64 }\n\
         fn main() -> Int64 {\n\
             do { Calc.adjust(41) } with { Calc.adjust(value) => resume(value + 1) }\n\
         }",
    );
    let handler = mir
        .functions()
        .iter()
        .find(|function| function.name == "main")
        .and_then(|function| {
            function
                .blocks
                .iter()
                .flat_map(|block| block.statements.iter())
                .find_map(|statement| match statement {
                    MirStatement::HandlerEnter { handlers } => handlers.first(),
                    _ => None,
                })
        })
        .expect("synthetic handler frame");
    let handler_function = handler
        .resumable_function
        .expect("capture-free handler should have a function id");
    let synthetic = mir
        .functions()
        .iter()
        .find(|function| function.id == handler_function)
        .expect("synthetic handler function should be in the program");
    assert_eq!(synthetic.parameters.len(), 1);
    assert_eq!(synthetic.parameters[0].ty, Type::I64);
    assert_eq!(synthetic.return_type, Type::I64);
    mir.verify()
        .expect("synthetic handler function should verify");
}

#[test]
fn lowers_multiple_immutable_resumable_handler_captures() {
    let mir = lower(
        "eff Calc { @resumable fn adjust(value: Int64) -> Int64 }\n\
         fn main() -> Int64 {\n\
             let offset: Int64 = 1\n\
             let scale: Int64 = 2\n\
             let bias: Int64 = 3\n\
             do { Calc.adjust(20) } with {\n\
                 Calc.adjust(value) => resume(value * scale + offset + bias)\n\
             }\n\
         }",
    );
    let handler = mir
        .functions()
        .iter()
        .find(|function| function.name == "main")
        .and_then(|function| {
            function
                .blocks
                .iter()
                .flat_map(|block| block.statements.iter())
                .find_map(|statement| match statement {
                    MirStatement::HandlerEnter { handlers } => handlers.first(),
                    _ => None,
                })
        })
        .expect("synthetic handler frame");
    assert_eq!(handler.resumable_captures.len(), 3);
    mir.verify()
        .expect("multiple synthetic handler captures should verify");
}

#[test]
fn lowers_class_resumable_handler_capture_as_borrowed() {
    let mir = lower(
        "class Counter { var value: Int32 = 41 }\n\
         eff Calc { @resumable fn adjust(value: Int32) -> Int32 }\n\
         fn main() -> Int32 {\n\
             let counter = Counter()\n\
             do { Calc.adjust(1) } with { Calc.adjust(value) => resume(counter.value + value) }\n\
         }",
    );
    let synthetic = mir
        .functions()
        .iter()
        .find(|candidate| candidate.name.starts_with("__handler_"))
        .expect("synthetic class handler function");
    let capture = synthetic
        .parameters
        .last()
        .expect("synthetic handler capture parameter");
    assert!(matches!(capture.ty, Type::Class(_)));
    assert_eq!(capture.ownership, MirOwnership::Borrowed);
    assert_eq!(
        synthetic.locals[capture.local.0].ownership,
        MirOwnership::Borrowed
    );
    mir.verify().expect("borrowed class capture should verify");
}

#[test]
fn verifier_rejects_borrowed_non_class_handler_capture() {
    let mut mir = lower(
        "eff Calc { @resumable fn adjust(value: Int64) -> Int64 }\n\
         fn main() -> Int64 {\n\
             let offset: Int64 = 1\n\
             do { Calc.adjust(20) } with { Calc.adjust(value) => resume(value + offset) }\n\
         }",
    );
    let function = mir
        .functions
        .iter_mut()
        .find(|function| function.name == "main")
        .expect("main MIR function");
    let capture = function
        .blocks
        .iter()
        .flat_map(|block| block.statements.iter())
        .find_map(|statement| match statement {
            MirStatement::HandlerEnter { handlers } => handlers
                .first()
                .and_then(|handler| handler.resumable_captures.first())
                .copied(),
            _ => None,
        })
        .expect("handler capture");
    function.value_ownership[capture.0] = MirOwnership::Borrowed;
    assert!(mir.verify().is_err());
}

#[test]
fn lowers_multiple_and_nested_handlers_with_nearest_frame_precedence() {
    let mir = lower(
        "eff Clock { fn now() -> Int64 }\n\
         fn main() -> Int64 {\n\
             let value = do { do { Clock.now() } with { Clock.now() => 1 } }\
                 with { Clock.now() => 2 };\
             value\n\
         }",
    );
    let enters = mir
        .functions()
        .iter()
        .flat_map(|function| function.blocks.iter())
        .flat_map(|block| block.statements.iter())
        .filter(|statement| matches!(statement, MirStatement::HandlerEnter { .. }))
        .count();
    assert_eq!(enters, 2);
    mir.verify().expect("nested handler CFG should verify");
}

#[test]
fn lowers_normal_handler_requests_inside_inherited_task_frames() {
    let mir = lower(
        "eff Console { fn read() -> Int64 }\n\
         fn ask() -> Int64 effects { Console } { Console.read() }\n\
         fn main() -> Int64 {\n\
             let values = do { parallel {\n\
                 | ask()\n\
                 | ask()\n\
             } }\
                 with { Console.read() => 41 };\n\
             values.0\n\
         }",
    );
    let task_functions = mir
        .functions()
        .iter()
        .filter(|function| function.name.starts_with("__task_main_"))
        .collect::<Vec<_>>();
    assert!(mir.functions().iter().any(|function| {
        function
            .blocks
            .iter()
            .flat_map(|block| &block.statements)
            .any(|statement| matches!(statement, MirStatement::HandlerRequest { .. }))
    }));
    assert!(task_functions
        .iter()
        .filter(|task| {
            task.blocks
                .iter()
                .flat_map(|block| &block.statements)
                .any(|statement| matches!(statement, MirStatement::HandlerRequest { .. }))
        })
        .all(|task| {
            !task
                .blocks
                .iter()
                .flat_map(|block| &block.statements)
                .any(|statement| matches!(statement, MirStatement::TaskAbort { .. }))
        }));
    mir.verify().expect("inherited handler task should verify");
}

#[test]
fn lowers_mixed_normal_and_abortive_handlers_without_normal_task_failure() {
    let mir = lower(
        "eff Console { fn read() -> Int32 }\n\
         eff Failure { @aborts fn stop() -> Unit }\n\
         fn main() -> Int32 { do { let value = Console.read(); Failure.stop(); value }\
             with { Console.read() => 41\n Failure.stop() => 7 } }",
    );
    let main = mir
        .functions()
        .iter()
        .find(|function| function.name == "main")
        .expect("main MIR function");
    let handlers = main
        .blocks
        .iter()
        .flat_map(|block| &block.statements)
        .find_map(|statement| match statement {
            MirStatement::HandlerEnter { handlers } => Some(handlers),
            _ => None,
        })
        .expect("mixed handler frame");
    assert_eq!(handlers.len(), 2);
    assert!(handlers
        .iter()
        .any(|handler| handler.resumable_value.is_some()));
    assert!(handlers
        .iter()
        .any(|handler| handler.resumable_value.is_none()));
    let body_task = mir
        .functions()
        .iter()
        .find(|function| function.is_task && function.name.starts_with("__task_main_"))
        .expect("mixed do body task");
    assert!(body_task
        .blocks
        .iter()
        .flat_map(|block| &block.statements)
        .any(|statement| matches!(statement, MirStatement::HandlerRequest { .. })));
    assert!(body_task
        .blocks
        .iter()
        .flat_map(|block| &block.statements)
        .any(|statement| matches!(statement, MirStatement::TaskAbort { .. })));
    mir.verify().expect("mixed handler MIR should verify");
}

#[test]
fn unmatched_normal_effect_uses_runtime_request_transport() {
    let mir = lower(
        "eff Clock { fn now() -> Int64 }\n\
         fn main() effects { Clock } { Clock.now() }",
    );
    assert!(mir.functions()[0].blocks.iter().any(|block| {
        block
            .statements
            .iter()
            .any(|statement| matches!(statement, MirStatement::HandlerRequest { .. }))
    }));
}

#[test]
fn verifier_rejects_invalid_handler_target() {
    let mut mir = lower(
        "eff Clock { fn now() -> Int64 }\n\
         fn main() -> Int64 { let value = do { Clock.now() } with { Clock.now() => 42 }; value }",
    );
    let function = &mut mir.functions[0];
    let handler = function
        .blocks
        .iter_mut()
        .flat_map(|block| block.statements.iter_mut())
        .find_map(|statement| match statement {
            MirStatement::HandlerEnter { handlers } => handlers.first_mut(),
            _ => None,
        })
        .expect("handler metadata");
    handler.target = MirBlockId(usize::MAX);
    assert!(mir.verify().is_err());
}

#[test]
fn verifier_rejects_handler_result_type_mismatch() {
    let mut mir = lower(
        "eff Clock { fn now() -> Int64 }\n\
         fn main() -> Int64 { let value = do { Clock.now() } with { Clock.now() => 42 }; value }",
    );
    let function = &mut mir.functions[0];
    let handler = function
        .blocks
        .iter_mut()
        .flat_map(|block| block.statements.iter_mut())
        .find_map(|statement| match statement {
            MirStatement::HandlerEnter { handlers } => handlers.first_mut(),
            _ => None,
        })
        .expect("handler metadata");
    handler.result_type = Type::Bool;
    assert!(mir.verify().is_err());
}

#[test]
fn verifier_rejects_abortive_handler_parameter_phi_mismatch() {
    let mut mir = lower(
        "eff Failure { @aborts fn stop(message: String) -> Int32 }\n\
         fn main() -> Int64 { let value = do { Failure.stop(message: \"bad\") } with { Failure.stop(message) => 42 }; value }",
    );
    let function = &mut mir.functions[0];
    let target = function
        .blocks
        .iter()
        .find(|block| {
            block
                .statements
                .iter()
                .any(|statement| matches!(statement, MirStatement::HandlerEnter { .. }))
        })
        .and_then(|block| {
            block
                .statements
                .iter()
                .find_map(|statement| match statement {
                    MirStatement::HandlerEnter { handlers } => {
                        handlers.first().map(|arm| arm.target)
                    }
                    _ => None,
                })
        })
        .expect("handler target");
    let phi = function.blocks[target.0]
        .statements
        .iter_mut()
        .find_map(|statement| match statement {
            MirStatement::Phi { incoming, .. } => Some(incoming),
            _ => None,
        })
        .expect("handler parameter phi");
    phi.clear();
    assert!(mir.verify().is_err());
}

#[test]
fn verifier_rejects_legacy_static_normal_handler_arm() {
    let mut mir = lower(
        "eff Clock { fn now() -> Int64 }\n\
         fn main() -> Int64 { let value = do { Clock.now() } with { Clock.now() => 42 }; value }",
    );
    let function = &mut mir.functions[0];
    let handler = function
        .blocks
        .iter_mut()
        .flat_map(|block| block.statements.iter_mut())
        .find_map(|statement| match statement {
            MirStatement::HandlerEnter { handlers } => handlers.first_mut(),
            _ => None,
        })
        .expect("handler metadata");
    handler.resumable_value = None;
    handler.resumable_function = None;
    handler.resumable_captures.clear();
    assert!(mir.verify().is_err());
}

#[test]
fn verifier_rejects_unbalanced_handler_exit() {
    let mut mir = lower(
        "eff Clock { fn now() -> Int64 }\n\
         fn main() -> Int64 { let value = do { Clock.now() } with { Clock.now() => 42 }; value }",
    );
    let function = &mut mir.functions[0];
    for block in &mut function.blocks {
        block
            .statements
            .retain(|statement| !matches!(statement, MirStatement::HandlerExit));
    }

    assert!(mir.verify().is_err());
}
