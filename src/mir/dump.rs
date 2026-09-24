use super::{MirFunction, MirStatement, MirTerminator};
use crate::sema::type_name;

impl MirFunction {
    #[allow(dead_code)]
    pub(crate) fn dump(&self) -> String {
        let mut output = format!("fn {} -> {} {{\n", self.name, type_name(self.return_type));
        for block in &self.blocks {
            output.push_str(&format!(
                "  bb{}{}:\n",
                block.id.0,
                if block.scoped { " [scope]" } else { "" }
            ));
            for statement in &block.statements {
                match statement {
                    MirStatement::Unit { destination } => {
                        output.push_str(&format!("    %{}: Unit = Unit\n", destination.0))
                    }
                    MirStatement::Const { destination, value } => {
                        output.push_str(&format!(
                            "    %{}: {} = const {:?}\n",
                            destination.0,
                            type_name(self.value_types[destination.0]),
                            value
                        ));
                    }
                    MirStatement::Read { destination, local } => {
                        output.push_str(&format!(
                            "    %{}: {} = read {}\n",
                            destination.0,
                            type_name(self.value_types[destination.0]),
                            local.0
                        ));
                    }
                    MirStatement::BorrowLocal { destination, local } => {
                        output.push_str(&format!(
                            "    %{}: {} = borrow {}\n",
                            destination.0,
                            type_name(self.value_types[destination.0]),
                            local.0
                        ));
                    }
                    MirStatement::TakeLocal { destination, local } => {
                        output.push_str(&format!(
                            "    %{}: {} = take {}\n",
                            destination.0,
                            type_name(self.value_types[destination.0]),
                            local.0
                        ));
                    }
                    MirStatement::Unary {
                        destination,
                        op,
                        operand,
                    } => output.push_str(&format!(
                        "    %{}: {} = {:?} %{}\n",
                        destination.0,
                        type_name(self.value_types[destination.0]),
                        op,
                        operand.0
                    )),
                    MirStatement::Binary {
                        destination,
                        op,
                        left,
                        right,
                    } => output.push_str(&format!(
                        "    %{}: {} = %{} {:?} %{}\n",
                        destination.0,
                        type_name(self.value_types[destination.0]),
                        left.0,
                        op,
                        right.0
                    )),
                    MirStatement::Numeric {
                        destination,
                        method,
                        arguments,
                    } => output.push_str(&format!(
                        "    %{}: {} = {:?} {:?}\n",
                        destination.0,
                        type_name(self.value_types[destination.0]),
                        method,
                        arguments
                    )),
                    MirStatement::Call {
                        destination,
                        function,
                        arguments,
                        continuation: _,
                    } => {
                        let arguments = arguments
                            .iter()
                            .map(|argument| {
                                format!("p{}: %{}", argument.parameter, argument.value.0)
                            })
                            .collect::<Vec<_>>()
                            .join(", ");
                        output.push_str(&format!(
                            "    %{}: {} = call {}({})\n",
                            destination.0,
                            type_name(self.value_types[destination.0]),
                            function.0,
                            arguments
                        ));
                    }
                    MirStatement::DynamicUpcast { destination, value } => {
                        output.push_str(&format!(
                            "    %{} = dyn-upcast %{}\n",
                            destination.0, value.0
                        ));
                    }
                    MirStatement::DynamicValue {
                        destination,
                        methods,
                        value,
                    } => {
                        output.push_str(&format!(
                            "    %{} = dyn %{} {:?}\n",
                            destination.0, value.0, methods
                        ));
                    }
                    MirStatement::FunctionValue {
                        destination,
                        function,
                        ..
                    } => {
                        output.push_str(&format!(
                            "    %{}: {} = function_value {}\n",
                            destination.0,
                            type_name(self.value_types[destination.0]),
                            function.0
                        ));
                    }
                    MirStatement::CallIndirect {
                        destination,
                        callee,
                        arguments,
                        ..
                    } => {
                        output.push_str(&format!(
                            "    %{}: {} = call_indirect %{}({})\n",
                            destination.0,
                            type_name(self.value_types[destination.0]),
                            callee.0,
                            arguments
                                .iter()
                                .map(|argument| format!("%{}", argument.value.0))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ));
                    }
                    MirStatement::Project {
                        destination,
                        base,
                        access,
                    } => output.push_str(&format!(
                        "    %{}: {} = project %{}.{:?}\n",
                        destination.0,
                        type_name(self.value_types[destination.0]),
                        base.0,
                        access
                    )),
                    MirStatement::EnumTag { destination, value } => output.push_str(&format!(
                        "    %{}: Int32 = enum_tag %{}\n",
                        destination.0, value.0
                    )),
                    MirStatement::EnumProject {
                        destination,
                        value,
                        variant,
                        field,
                    } => output.push_str(&format!(
                        "    %{}: {} = enum_project %{} variant {} field {}\n",
                        destination.0,
                        type_name(self.value_types[destination.0]),
                        value.0,
                        variant,
                        field
                    )),
                    MirStatement::Tuple {
                        destination,
                        elements,
                    } => output.push_str(&format!(
                        "    %{}: {} = tuple ({})\n",
                        destination.0,
                        type_name(self.value_types[destination.0]),
                        elements
                            .iter()
                            .map(|value| format!("%{}", value.0))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )),
                    MirStatement::EnumConstruct {
                        destination,
                        enum_id,
                        variant,
                        arguments,
                    } => output.push_str(&format!(
                        "    %{}: {} = enum {}.{}({})\n",
                        destination.0,
                        type_name(self.value_types[destination.0]),
                        format_args!("{:?}", enum_id),
                        variant,
                        arguments
                            .iter()
                            .map(|argument| format!("%{}", argument.value.0))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )),
                    MirStatement::Construct {
                        destination,
                        type_id,
                        fields,
                    } => output.push_str(&format!(
                        "    %{}: {} = construct {}({})\n",
                        destination.0,
                        type_name(self.value_types[destination.0]),
                        format_args!("{:?}", type_id),
                        fields
                            .iter()
                            .map(|value| format!("%{}", value.0))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )),
                    MirStatement::Store {
                        destination,
                        receiver,
                        access,
                        value,
                    } => output.push_str(&format!(
                        "    %{}: Unit = store %{}.{:?} <- %{}\n",
                        destination.0, receiver.0, access, value.0
                    )),
                    MirStatement::MethodCall {
                        destination,
                        receiver,
                        method,
                        arguments,
                        ..
                    } => output.push_str(&format!(
                        "    %{}: {} = method %{} .{}({})\n",
                        destination.0,
                        type_name(self.value_types[destination.0]),
                        receiver.0,
                        method.0,
                        arguments
                            .iter()
                            .map(|argument| format!("%{}", argument.value.0))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )),
                    MirStatement::RuntimeCall {
                        destination,
                        intrinsic,
                        arguments,
                    } => output.push_str(&format!(
                        "    %{}: {} = runtime {:?}{}({})\n",
                        destination.0,
                        type_name(self.value_types[destination.0]),
                        intrinsic,
                        intrinsic
                            .runtime_symbol()
                            .map(|symbol| format!(" [{symbol}]"))
                            .unwrap_or_default(),
                        arguments
                            .iter()
                            .map(|argument| format!("%{}", argument.value.0))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )),
                    MirStatement::ResumableRequest {
                        destination,
                        operation,
                        continuation,
                        arguments,
                    } => output.push_str(&format!(
                        "    %{}: {} = resumable c{} {:?}({})\n",
                        destination.0,
                        type_name(self.value_types[destination.0]),
                        continuation.0,
                        operation,
                        arguments
                            .iter()
                            .map(|argument| format!("%{}", argument.value.0))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )),
                    MirStatement::HandlerRequest {
                        destination,
                        operation,
                        continuation,
                        arguments,
                    } => output.push_str(&format!(
                        "    %{}: {} = handler c{} {:?}({})\n",
                        destination.0,
                        type_name(self.value_types[destination.0]),
                        continuation.0,
                        operation,
                        arguments
                            .iter()
                            .map(|argument| format!("%{}", argument.value.0))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )),
                    MirStatement::Suspend {
                        destination,
                        operation,
                        continuation,
                        arguments,
                    } => output.push_str(&format!(
                        "    %{}: {} = suspend c{} {:?}({})\n",
                        destination.0,
                        type_name(self.value_types[destination.0]),
                        continuation.0,
                        operation,
                        arguments
                            .iter()
                            .map(|argument| format!("%{}", argument.value.0))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )),
                    MirStatement::Resume { continuation } => {
                        output.push_str(&format!("    resume c{}\n", continuation.0))
                    }
                    MirStatement::TaskPoll { destination } => {
                        output.push_str(&format!("    %{}: Bool = task_poll\n", destination.0))
                    }
                    MirStatement::TaskCancelled { destination } => output.push_str(&format!(
                        "    %{}: {} = task_cancelled\n",
                        destination.0,
                        type_name(self.value_types[destination.0])
                    )),
                    MirStatement::TaskAbort {
                        destination,
                        operation,
                        arguments,
                    } => output.push_str(&format!(
                        "    %{}: {} = task_abort {:?}({})\n",
                        destination.0,
                        type_name(self.value_types[destination.0]),
                        operation,
                        arguments
                            .iter()
                            .map(|value| format!("%{}", value.0))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )),
                    MirStatement::TaskFailureOperation { destination, scope } => {
                        output.push_str(&format!(
                            "    %{}: U64 = task_failure_operation s{}\n",
                            destination.0, scope.0
                        ))
                    }
                    MirStatement::TaskFailurePayload {
                        destination,
                        scope,
                        operation,
                        parameter,
                    } => output.push_str(&format!(
                        "    %{}: {} = task_failure_payload s{} {:?}[{}]\n",
                        destination.0,
                        type_name(self.value_types[destination.0]),
                        scope.0,
                        operation,
                        parameter
                    )),
                    MirStatement::TaskFailureRethrow { destination, scope } => {
                        output.push_str(&format!(
                            "    %{}: {} = task_failure_rethrow s{}\n",
                            destination.0,
                            type_name(self.value_types[destination.0]),
                            scope.0
                        ))
                    }
                    MirStatement::HandlerEnter { handlers } => output.push_str(&format!(
                        "    handler_enter [{}]\n",
                        handlers
                            .iter()
                            .map(|handler| format!(
                                "{:?} -> bb{} (join bb{}, parameter indices {:?}, locals {:?})",
                                handler.operation,
                                handler.target.0,
                                handler.join.0,
                                handler.parameter_indices,
                                handler.parameter_locals
                            ))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )),
                    MirStatement::HandlerExit => output.push_str("    handler_exit\n"),
                    MirStatement::ScopeEnter { scope, region } => {
                        let operation = if *region {
                            "region_enter"
                        } else {
                            "scope_enter"
                        };
                        output.push_str(&format!("    {operation} s{}\n", scope.0))
                    }
                    MirStatement::CownAcquire {
                        wait_for_change,
                        destination,
                        arguments,
                        continuation,
                    } => {
                        output.push_str(&format!(
                            "    %{}: Unit = {} c{}({})\n",
                            destination.0,
                            if *wait_for_change {
                                "cown_wait_change"
                            } else {
                                "cown_acquire"
                            },
                            continuation.0,
                            arguments
                                .iter()
                                .map(|arg| format!("%{}", arg.value.0))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ));
                    }
                    MirStatement::TaskWait {
                        destination,
                        scope,
                        race,
                        continuation,
                    } => {
                        output.push_str(&format!(
                            "    %{} = task_wait s{} race={} c{}\n",
                            destination.0, scope.0, race, continuation.0
                        ));
                    }
                    MirStatement::ScopeExit { scope } => {
                        output.push_str(&format!("    scope_exit s{}\n", scope.0))
                    }
                    MirStatement::TaskCreate {
                        scope,
                        task,
                        function,
                        arguments,
                    } => output.push_str(&format!(
                        "    task t{} = create s{} fn{}({})\n",
                        task.0,
                        scope.0,
                        function.0,
                        arguments
                            .iter()
                            .map(|argument| format!("%{}", argument.value.0))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )),
                    MirStatement::TaskJoin {
                        destination,
                        scope,
                        task,
                    } => output.push_str(&format!(
                        "    %{}: {} = join s{} t{}\n",
                        destination.0,
                        type_name(self.value_types[destination.0]),
                        scope.0,
                        task.0
                    )),
                    MirStatement::TaskClaimResult { scope, task } => {
                        output.push_str(&format!("    claim_result s{} t{}\n", scope.0, task.0))
                    }
                    MirStatement::TaskCancel { scope, task } => {
                        output.push_str(&format!("    cancel s{} t{}\n", scope.0, task.0))
                    }
                    MirStatement::RaceStart { scope, tasks } => output.push_str(&format!(
                        "    race_start s{} [{}]\n",
                        scope.0,
                        tasks
                            .iter()
                            .map(|task| format!("t{}", task.0))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )),
                    MirStatement::RaceSelect {
                        destination,
                        scope,
                        tasks,
                    } => output.push_str(&format!(
                        "    %{}: {} = race_select s{} [{}]\n",
                        destination.0,
                        type_name(self.value_types[destination.0]),
                        scope.0,
                        tasks
                            .iter()
                            .map(|task| format!("t{}", task.0))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )),
                    MirStatement::Dup { destination, value } => output.push_str(&format!(
                        "    %{}: {} = dup %{}\n",
                        destination.0,
                        type_name(self.value_types[destination.0]),
                        value.0
                    )),
                    MirStatement::Move { destination, value } => output.push_str(&format!(
                        "    %{}: {} = move %{}\n",
                        destination.0,
                        type_name(self.value_types[destination.0]),
                        value.0
                    )),
                    MirStatement::Drop { destination, value } => output.push_str(&format!(
                        "    %{}: Unit = drop %{}\n",
                        destination.0, value.0
                    )),
                    MirStatement::Deinit {
                        destination,
                        value,
                        variant,
                    } => {
                        let variant = variant
                            .map(|variant| format!(" variant {variant}"))
                            .unwrap_or_default();
                        output.push_str(&format!(
                            "    %{}: Unit = deinit %{}{}\n",
                            destination.0, value.0, variant
                        ));
                    }
                    MirStatement::TaskFailureClaim { scope } => {
                        output.push_str(&format!("    task_failure_claim s{}\n", scope.0));
                    }
                    MirStatement::DropLocal { destination, local } => output.push_str(&format!(
                        "    %{}: Unit = drop_local {}\n",
                        destination.0, local.0
                    )),
                    MirStatement::Bind {
                        local,
                        value,
                        destination,
                    } => {
                        let source = value
                            .map_or_else(|| "Unit".to_owned(), |value| format!("%{}", value.0));
                        output.push_str(&format!(
                            "    %{}: Unit = bind {} <- {}\n",
                            destination.0, local.0, source
                        ));
                    }
                    MirStatement::Phi {
                        destination,
                        incoming,
                    } => {
                        let incoming = incoming
                            .iter()
                            .map(|(block, value)| format!("bb{}:%{}", block.0, value.0))
                            .collect::<Vec<_>>()
                            .join(", ");
                        output.push_str(&format!(
                            "    %{}: {} = phi [{}]\n",
                            destination.0,
                            type_name(self.value_types[destination.0]),
                            incoming
                        ));
                    }
                }
            }
            if let Some(terminator) = &block.terminator {
                output.push_str("    ");
                match terminator {
                    MirTerminator::Goto { target, arguments } => {
                        output.push_str(&format!("goto bb{}", target.0));
                        if !arguments.is_empty() {
                            let args = arguments
                                .iter()
                                .map(|value| format!("%{}", value.0))
                                .collect::<Vec<_>>()
                                .join(", ");
                            output.push_str(&format!("({args})"));
                        }
                    }
                    MirTerminator::Branch {
                        condition,
                        then_block,
                        else_block,
                    } => output.push_str(&format!(
                        "branch %{} -> bb{}, bb{}",
                        condition.0, then_block.0, else_block.0
                    )),
                    MirTerminator::Return(value) => match value {
                        Some(value) => output.push_str(&format!("return %{}", value.0)),
                        None => output.push_str("return"),
                    },
                    MirTerminator::Unreachable => output.push_str("unreachable"),
                }
                output.push('\n');
            }
        }
        output.push('}');
        output
    }
}
