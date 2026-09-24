use super::super::*;
use crate::hir::{CoreCallArgument, CoreExpr, CoreExprKind};

impl Lowerer<'_> {
    pub(super) fn lower_cown_new(
        &mut self,
        expression: &CoreExpr,
        arguments: &[CoreCallArgument],
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let Some(payload) = self.lower_value(&arguments[0].value, function_names, types)? else {
            return Ok(None);
        };
        let destination = self.next_value(expression.ty);
        self.push_statement(MirStatement::RuntimeCall {
            destination,
            intrinsic: RuntimeIntrinsic::CownNew(self.value_types[payload.0]),
            arguments: vec![MirCallArgument {
                parameter: 0,
                value: payload,
            }],
        });
        Ok(Some(destination))
    }

    pub(super) fn lower_bytes_from_string(
        &mut self,
        expression: &CoreExpr,
        arguments: &[CoreCallArgument],
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let Some(value) = self.lower_value(&arguments[0].value, function_names, types)? else {
            return Ok(None);
        };
        let destination = self.next_value(expression.ty);
        self.push_statement(MirStatement::RuntimeCall {
            destination,
            intrinsic: RuntimeIntrinsic::BytesFromString,
            arguments: vec![MirCallArgument {
                parameter: 0,
                value,
            }],
        });
        Ok(Some(destination))
    }

    pub(super) fn lower_mut_bytes_constructor(
        &mut self,
        expression: &CoreExpr,
        callee: &CoreExpr,
        arguments: &[CoreCallArgument],
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let CoreExprKind::Field {
            access: crate::syntax::FieldAccess::Name(method),
            ..
        } = &callee.kind
        else {
            unreachable!("MutBytes constructor guard requires a named field");
        };
        let Some(value) = self.lower_value(&arguments[0].value, function_names, types)? else {
            return Ok(None);
        };
        let destination = self.next_value(expression.ty);
        self.push_statement(MirStatement::RuntimeCall {
            destination,
            intrinsic: if method == "with_capacity" {
                RuntimeIntrinsic::MutBytesWithCapacity
            } else {
                RuntimeIntrinsic::MutBytesFromBytes
            },
            arguments: vec![MirCallArgument {
                parameter: 0,
                value,
            }],
        });
        Ok(Some(destination))
    }

    pub(super) fn lower_print(
        &mut self,
        callee: &CoreExpr,
        arguments: &[CoreCallArgument],
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let is_print = matches!(&callee.kind, CoreExprKind::Name(name) if name == "print");
        if matches!(arguments[0].value.ty, Type::Dyn(id) if types.dynamic_types[id].includes("Show"))
        {
            let method = CoreExpr {
                id: callee.id,
                ty: Type::Unit,
                kind: CoreExprKind::Field {
                    value: Box::new(arguments[0].value.clone()),
                    access: FieldAccess::Name(crate::sema::SHOW_METHOD.into()),
                },
            };
            let call = CoreExpr {
                id: callee.id,
                ty: Type::String,
                kind: CoreExprKind::Unit,
            };
            let Some(displayed) =
                self.lower_dynamic_call(&call, &method, &[], function_names, types)?
            else {
                return Ok(None);
            };
            let destination = self.next_value(Type::Unit);
            self.push_statement(MirStatement::RuntimeCall {
                destination,
                intrinsic: if is_print {
                    RuntimeIntrinsic::Print
                } else {
                    RuntimeIntrinsic::Println
                },
                arguments: vec![MirCallArgument {
                    parameter: 0,
                    value: displayed,
                }],
            });
            return Ok(Some(destination));
        }
        if types.has_show_impl(arguments[0].value.ty) {
            let Some(receiver) =
                self.lower_borrowed_value(&arguments[0].value, function_names, types)?
            else {
                return Ok(None);
            };
            let method_id = *self
                .function_ids
                .get(&method_key(arguments[0].value.ty, crate::sema::SHOW_METHOD))
                .ok_or_else(|| Diagnostic::codegen("MIR Show method was not resolved"))?;
            let displayed = self.next_value(Type::String);
            self.push_statement(MirStatement::MethodCall {
                destination: displayed,
                receiver,
                receiver_type: arguments[0].value.ty,
                method: method_id,
                arguments: Vec::new(),
                continuation: None, // Phase 2: will be added in post-processing
            });
            let destination = self.next_value(Type::Unit);
            self.push_statement(MirStatement::RuntimeCall {
                destination,
                intrinsic: if is_print {
                    RuntimeIntrinsic::Print
                } else {
                    RuntimeIntrinsic::Println
                },
                arguments: vec![MirCallArgument {
                    parameter: 0,
                    value: displayed,
                }],
            });
            return Ok(Some(destination));
        }
        let Some(value) = self.lower_value(&arguments[0].value, function_names, types)? else {
            return Ok(None);
        };
        if arguments[0].value.ty == Type::String {
            let destination = self.next_value(Type::Unit);
            self.push_statement(MirStatement::RuntimeCall {
                destination,
                intrinsic: if is_print {
                    RuntimeIntrinsic::Print
                } else {
                    RuntimeIntrinsic::Println
                },
                arguments: vec![MirCallArgument {
                    parameter: 0,
                    value,
                }],
            });
            return Ok(Some(destination));
        }
        let displayed = self.next_value(Type::String);
        self.push_statement(MirStatement::RuntimeCall {
            destination: displayed,
            intrinsic: RuntimeIntrinsic::Show(arguments[0].value.ty),
            arguments: vec![MirCallArgument {
                parameter: 0,
                value,
            }],
        });
        let destination = self.next_value(Type::Unit);
        self.push_statement(MirStatement::RuntimeCall {
            destination,
            intrinsic: if is_print {
                RuntimeIntrinsic::Print
            } else {
                RuntimeIntrinsic::Println
            },
            arguments: vec![MirCallArgument {
                parameter: 0,
                value: displayed,
            }],
        });
        Ok(Some(destination))
    }

    pub(super) fn lower_numeric_method(
        &mut self,
        expression: &CoreExpr,
        callee: &CoreExpr,
        arguments: &[CoreCallArgument],
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let CoreExprKind::Field {
            value: receiver,
            access: crate::syntax::FieldAccess::Name(method),
        } = &callee.kind
        else {
            return Err(Diagnostic::codegen("numeric method was not resolved"));
        };
        let Some(receiver) = self.lower_value(receiver, function_names, types)? else {
            return Ok(None);
        };
        let method = match method.as_str() {
            "abs" => crate::mir::NumericMethod::Abs,
            "min" => crate::mir::NumericMethod::Min,
            "max" => crate::mir::NumericMethod::Max,
            _ => return Err(Diagnostic::codegen("numeric method was not resolved")),
        };
        let mut operands = vec![receiver];
        if matches!(
            method,
            crate::mir::NumericMethod::Min | crate::mir::NumericMethod::Max
        ) {
            let Some(argument) = arguments.first() else {
                return Err(Diagnostic::codegen("min/max is missing an operand"));
            };
            let Some(value) = self.lower_value(&argument.value, function_names, types)? else {
                return Ok(None);
            };
            operands.push(value);
        }
        if !self.is_open() {
            return Ok(None);
        }
        let destination = self.next_value(expression.ty);
        self.push_statement(MirStatement::Numeric {
            destination,
            method,
            arguments: operands,
        });
        Ok(Some(destination))
    }

    pub(super) fn lower_panic(
        &mut self,
        arguments: &[CoreCallArgument],
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let Some(message) = self.lower_value(&arguments[0].value, function_names, types)? else {
            return Ok(None);
        };
        let destination = self.next_value(Type::Unit);
        self.push_statement(MirStatement::RuntimeCall {
            destination,
            intrinsic: RuntimeIntrinsic::Panic,
            arguments: vec![MirCallArgument {
                parameter: 0,
                value: message,
            }],
        });
        for scope in self.task_scopes.clone().into_iter().rev() {
            self.push_statement(MirStatement::ScopeExit { scope });
        }
        self.terminate(MirTerminator::Unreachable)?;
        Ok(None)
    }

    pub(super) fn lower_string_parse(
        &mut self,
        expression: &CoreExpr,
        callee: &CoreExpr,
        type_arguments: &[Type],
        arguments: &[CoreCallArgument],
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let CoreExprKind::Field { value, .. } = &callee.kind else {
            unreachable!("String parse guard requires a field");
        };
        let Some(receiver) = self.lower_value(value, function_names, types)? else {
            return Ok(None);
        };
        let destination = self.next_value(expression.ty);
        self.push_statement(MirStatement::RuntimeCall {
            destination,
            intrinsic: RuntimeIntrinsic::StringParse(type_arguments[0]),
            arguments: vec![MirCallArgument {
                parameter: 0,
                value: receiver,
            }],
        });
        let _ = arguments;
        Ok(Some(destination))
    }

    pub(super) fn lower_string_as_cstr(
        &mut self,
        callee: &CoreExpr,
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let CoreExprKind::Field {
            value: receiver, ..
        } = &callee.kind
        else {
            unreachable!("String as_cstr guard requires a named field");
        };
        let Some(receiver) = self.lower_value(receiver, function_names, types)? else {
            return Ok(None);
        };
        // Park the receiver's reference in a scope-lived local: the lent
        // CString borrows the managed payload, so the String must outlive the
        // statement even when the receiver is an anonymous temporary. The
        // scope-exit DropLocal releases the lease.
        let lease = self.new_local("<c-string-lease>", Type::String);
        let bind = self.next_value(Type::Unit);
        self.push_statement(MirStatement::Bind {
            local: lease,
            value: Some(receiver),
            destination: bind,
        });
        // Read carries no reference bump; codegen must not drop the argument.
        let borrowed = self.next_value(Type::String);
        self.push_statement(MirStatement::Read {
            destination: borrowed,
            local: lease,
        });
        let destination = self.next_value(Type::CStr);
        self.push_statement(MirStatement::RuntimeCall {
            destination,
            intrinsic: RuntimeIntrinsic::StringAsCString,
            arguments: vec![MirCallArgument {
                parameter: 0,
                value: borrowed,
            }],
        });
        Ok(Some(destination))
    }

    pub(super) fn lower_string_method(
        &mut self,
        expression: &CoreExpr,
        callee: &CoreExpr,
        arguments: &[CoreCallArgument],
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let CoreExprKind::Field {
            value: receiver,
            access: crate::syntax::FieldAccess::Name(method),
        } = &callee.kind
        else {
            unreachable!("String method call guard requires a named field");
        };
        let Some(receiver) = self.lower_value(receiver, function_names, types)? else {
            return Ok(None);
        };
        let intrinsic = match method.as_str() {
            "is_empty" => RuntimeIntrinsic::StringIsEmpty,
            "byte_count" => RuntimeIntrinsic::StringByteCount,
            "starts_with" => RuntimeIntrinsic::StringStartsWith,
            "ends_with" => RuntimeIntrinsic::StringEndsWith,
            "contains" => RuntimeIntrinsic::StringContains,
            "scalar_count" => RuntimeIntrinsic::StringScalarCount,
            "grapheme_count" | "length" => RuntimeIntrinsic::StringGraphemeCount,
            "is_ascii" => RuntimeIntrinsic::StringIsAscii,
            "concat" => RuntimeIntrinsic::StringConcat,
            "trim" => RuntimeIntrinsic::StringTrim,
            "to_upper" => RuntimeIntrinsic::StringToUpper,
            "to_lower" => RuntimeIntrinsic::StringToLower,
            "split" => RuntimeIntrinsic::StringSplit,
            "replace" => RuntimeIntrinsic::StringReplace,
            "get_byte" => RuntimeIntrinsic::StringGetByte,
            "slice" => RuntimeIntrinsic::StringSlice,
            _ => return Err(Diagnostic::codegen("MIR String method was not resolved")),
        };
        let destination = self.next_value(expression.ty);
        let mut intrinsic_arguments = vec![MirCallArgument {
            parameter: 0,
            value: receiver,
        }];
        for (index, argument) in arguments.iter().enumerate() {
            let Some(value) = self.lower_value(&argument.value, function_names, types)? else {
                return Ok(None);
            };
            intrinsic_arguments.push(MirCallArgument {
                parameter: index + 1,
                value,
            });
        }
        self.push_statement(MirStatement::RuntimeCall {
            destination,
            intrinsic,
            arguments: intrinsic_arguments,
        });
        Ok(Some(destination))
    }
}
