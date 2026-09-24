use super::collections::is_collection_call;
use super::*;
use crate::hir::{CoreExpr, CoreExprKind};

impl Lowerer<'_> {
    pub(super) fn lower_call(
        &mut self,
        expression: &CoreExpr,
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        if let CoreExprKind::Call {
            callee, arguments, ..
        } = &expression.kind
        {
            if let CoreExprKind::Name(name) = &callee.kind {
                if let Some(op) = crate::sema::path_intrinsics::operation(name) {
                    let mut operands = Vec::new();
                    for (parameter, argument) in arguments.iter().enumerate() {
                        let Some(value) =
                            self.lower_value(&argument.value, function_names, types)?
                        else {
                            return Ok(None);
                        };
                        operands.push(MirCallArgument { parameter, value });
                    }
                    let destination = self.next_value(expression.ty);
                    self.push_statement(MirStatement::RuntimeCall {
                        destination,
                        intrinsic: RuntimeIntrinsic::Path(op),
                        arguments: operands,
                    });
                    return Ok(Some(destination));
                }
            }
        }
        match &expression.kind {
            CoreExprKind::Call {
                callee, arguments, ..
            } if matches!(&callee.kind, CoreExprKind::Field { value, access: FieldAccess::Name(method) }
                    if value.ty.is_numeric() && matches!(method.as_str(), "abs" | "min" | "max")) =>
            {
                self.lower_numeric_method(expression, callee, arguments, function_names, types)
            }
            CoreExprKind::Call { callee, .. }
                if matches!(&callee.kind, CoreExprKind::Field { value, access: FieldAccess::Name(method) }
                    if matches!(value.ty, Type::List(_)) && method == crate::sema::CURSOR_METHOD) =>
            {
                let CoreExprKind::Field { value, .. } = &callee.kind else {
                    unreachable!()
                };
                self.lower_list_advance(expression, value, function_names, types)
            }
            CoreExprKind::Call { callee, .. }
                if matches!(&callee.kind, CoreExprKind::Field { value, access: FieldAccess::Name(method) }
                    if value.ty == Type::BytesCursor && method == crate::sema::CURSOR_METHOD) =>
            {
                let CoreExprKind::Field { value, .. } = &callee.kind else {
                    unreachable!()
                };
                self.lower_bytes_cursor_advance(expression, value, function_names, types)
            }
            CoreExprKind::Call { callee, .. }
                if matches!(&callee.kind, CoreExprKind::Field { value, access: FieldAccess::Name(method) }
                    if matches!(value.ty, Type::MapCursor(_) | Type::MapKeyCursor(_) | Type::MapValueCursor(_))
                        && method == crate::sema::CURSOR_METHOD) =>
            {
                let CoreExprKind::Field { value, .. } = &callee.kind else {
                    unreachable!()
                };
                self.lower_map_cursor_advance(expression, value, function_names, types)
            }
            CoreExprKind::Call { callee, .. }
                if matches!(&callee.kind, CoreExprKind::Field { value, access: FieldAccess::Name(method) }
                    if matches!(value.ty, Type::MutListCursor(_) | Type::MutMapCursor(_) | Type::MutSetCursor(_))
                        && method == crate::sema::CURSOR_METHOD) =>
            {
                let CoreExprKind::Field { value, .. } = &callee.kind else {
                    unreachable!()
                };
                self.lower_mut_cursor_advance(expression, value, function_names, types)
            }
            CoreExprKind::Call { callee, .. }
                if matches!(&callee.kind, CoreExprKind::Field { value, access: FieldAccess::Name(method) }
                    if types.range_item(value.ty).is_some() && method == crate::sema::CURSOR_METHOD) =>
            {
                let CoreExprKind::Field { value, .. } = &callee.kind else {
                    unreachable!()
                };
                self.lower_range_advance(expression, value, function_names, types)
            }
            CoreExprKind::Call { callee, .. } if matches!(&callee.kind, CoreExprKind::Name(name) if name == "Hasher") =>
            {
                let destination = self.next_value(Type::Hasher);
                self.push_statement(MirStatement::RuntimeCall {
                    destination,
                    intrinsic: RuntimeIntrinsic::HasherNew,
                    arguments: vec![],
                });
                Ok(Some(destination))
            }
            CoreExprKind::Call {
                callee, arguments, ..
            } if matches!(&callee.kind, CoreExprKind::Field { value, access: FieldAccess::Name(method) }
                    if (value.ty == Type::Hasher && method == "finish") || (method == crate::sema::HASH_METHOD && types.has_builtin_hash(value.ty))) =>
            {
                let CoreExprKind::Field {
                    value: receiver, ..
                } = &callee.kind
                else {
                    unreachable!()
                };
                if matches!(receiver.ty, Type::Tuple(_)) {
                    return self.lower_tuple_hash(
                        receiver,
                        &arguments[0].value,
                        function_names,
                        types,
                    );
                }
                if matches!(receiver.ty, Type::Enum(_)) {
                    return self.lower_enum_hash(
                        receiver,
                        &arguments[0].value,
                        function_names,
                        types,
                    );
                }
                let Some(value) = self.lower_borrowed_value(receiver, function_names, types)?
                else {
                    return Ok(None);
                };
                let mut operands = vec![MirCallArgument {
                    parameter: 0,
                    value,
                }];
                for (index, argument) in arguments.iter().enumerate() {
                    let Some(value) =
                        self.lower_borrowed_value(&argument.value, function_names, types)?
                    else {
                        return Ok(None);
                    };
                    operands.push(MirCallArgument {
                        parameter: index + 1,
                        value,
                    });
                }
                let destination = self.next_value(expression.ty);
                self.push_statement(MirStatement::RuntimeCall {
                    destination,
                    intrinsic: if receiver.ty == Type::Hasher {
                        RuntimeIntrinsic::HasherFinish
                    } else {
                        RuntimeIntrinsic::Hash(receiver.ty)
                    },
                    arguments: operands,
                });
                Ok(Some(destination))
            }
            CoreExprKind::Call { callee, .. }
                if matches!(&callee.kind, CoreExprKind::Field { value, access: FieldAccess::Name(method) }
                    if matches!((value.ty, method.as_str()), (Type::List(_), "sorted") | (Type::MutList(_), "sort"))) =>
            {
                let CoreExprKind::Field { value, .. } = &callee.kind else {
                    unreachable!()
                };
                self.lower_named_call(
                    expression,
                    &crate::hir::sort_function_name(value.ty),
                    &[crate::hir::CoreCallArgument {
                        label: None,
                        value: *value.clone(),
                    }],
                    function_names,
                    types,
                )
            }
            CoreExprKind::Call {
                callee, arguments, ..
            } if matches!(&callee.kind, CoreExprKind::Field { value, access: FieldAccess::Name(method) }
                    if matches!(value.ty, Type::MutList(_) | Type::MutMap(_) | Type::MutSet(_))
                        && method == "retain") =>
            {
                let CoreExprKind::Field { value, .. } = &callee.kind else {
                    unreachable!()
                };
                self.lower_named_call(
                    expression,
                    &crate::hir::retain_function_name(value.ty),
                    &[
                        crate::hir::CoreCallArgument {
                            label: None,
                            value: *value.clone(),
                        },
                        arguments[0].clone(),
                    ],
                    function_names,
                    types,
                )
            }
            CoreExprKind::Call {
                callee, arguments, ..
            } if matches!(&callee.kind, CoreExprKind::Name(name) if name.starts_with("@ordering/")) =>
            {
                let CoreExprKind::Name(name) = &callee.kind else {
                    unreachable!()
                };
                self.lower_ordering_test(name, &arguments[0].value, function_names, types)
            }
            CoreExprKind::Call {
                callee, arguments, ..
            } if matches!(&callee.kind, CoreExprKind::Field { value, access: FieldAccess::Name(method) }
                    if (method == crate::sema::PARTIAL_ORD_METHOD || method == crate::sema::ORD_METHOD)
                        && types.has_builtin_ordering(value.ty, method == crate::sema::ORD_METHOD)) =>
            {
                let CoreExprKind::Field {
                    value: receiver,
                    access: FieldAccess::Name(method),
                } = &callee.kind
                else {
                    unreachable!()
                };
                if types.comparison_members(receiver.ty).is_some() {
                    return self.lower_aggregate_ordering(
                        receiver,
                        &arguments[0].value,
                        method == crate::sema::PARTIAL_ORD_METHOD,
                        function_names,
                        types,
                    );
                }
                let Some(left) = self.lower_value(receiver, function_names, types)? else {
                    return Ok(None);
                };
                let Some(right) = self.lower_value(&arguments[0].value, function_names, types)?
                else {
                    return Ok(None);
                };
                let destination = self.next_value(expression.ty);
                self.push_statement(MirStatement::RuntimeCall {
                    destination,
                    intrinsic: if method == crate::sema::ORD_METHOD {
                        RuntimeIntrinsic::Compare(receiver.ty)
                    } else {
                        RuntimeIntrinsic::PartialCompare(receiver.ty)
                    },
                    arguments: vec![
                        MirCallArgument {
                            parameter: 0,
                            value: left,
                        },
                        MirCallArgument {
                            parameter: 1,
                            value: right,
                        },
                    ],
                });
                Ok(Some(destination))
            }
            CoreExprKind::Call {
                callee, arguments, ..
            } if compares_option_with_none(callee, arguments) => {
                let CoreExprKind::Field {
                    value: receiver, ..
                } = &callee.kind
                else {
                    unreachable!()
                };
                self.lower_option_none_equality(
                    receiver,
                    &arguments[0].value,
                    function_names,
                    types,
                )
            }
            CoreExprKind::Call {
                callee, arguments, ..
            } if matches!(&callee.kind, CoreExprKind::Field { value, access: FieldAccess::Name(method) }
                    if method == crate::sema::PARTIAL_EQ_METHOD && types.has_builtin_partial_eq(value.ty)) =>
            {
                let CoreExprKind::Field {
                    value: receiver, ..
                } = &callee.kind
                else {
                    unreachable!()
                };
                if types.comparison_members(receiver.ty).is_some() {
                    return self.lower_aggregate_equality(
                        receiver,
                        &arguments[0].value,
                        function_names,
                        types,
                    );
                }
                let Some(left) = self.lower_value(receiver, function_names, types)? else {
                    return Ok(None);
                };
                let Some(right) = self.lower_value(&arguments[0].value, function_names, types)?
                else {
                    return Ok(None);
                };
                let destination = self.next_value(Type::Bool);
                if receiver.ty.is_numeric() || receiver.ty == Type::Bool {
                    // Preserve scalar constant folding after trait selection.
                    self.push_statement(MirStatement::Binary {
                        destination,
                        op: BinaryOp::Equal,
                        left,
                        right,
                    });
                } else {
                    self.push_statement(MirStatement::RuntimeCall {
                        destination,
                        intrinsic: RuntimeIntrinsic::PartialEqual(receiver.ty),
                        arguments: vec![
                            MirCallArgument {
                                parameter: 0,
                                value: left,
                            },
                            MirCallArgument {
                                parameter: 1,
                                value: right,
                            },
                        ],
                    });
                }
                Ok(Some(destination))
            }
            CoreExprKind::Call {
                callee, arguments, ..
            } if matches!(&callee.kind, CoreExprKind::Name(name) if name == "@debug") => {
                self.lower_debug(&arguments[0].value, function_names, types)
            }
            CoreExprKind::Call { callee, .. } if matches!(&callee.kind, CoreExprKind::Name(name) if name == "@debug/body") => {
                self.lower_default_debug_body(expression.id, function_names, types)
            }
            CoreExprKind::Call {
                callee, arguments, ..
            } if matches!(&callee.kind, CoreExprKind::Name(name) if name == "echo") => {
                self.lower_echo(arguments, false, function_names, types)
            }
            CoreExprKind::Call { callee, .. }
                if matches!(&callee.kind, CoreExprKind::Field { value, access: FieldAccess::Name(method) }
                    if method == crate::sema::DEBUG_METHOD && (!matches!(value.ty, Type::Struct(_) | Type::Class(_) | Type::Dyn(_))
                        || (types.has_default_debug(value.ty)
                            && !types.interface.method_symbols.contains_key(&(value.ty, crate::sema::DEBUG_METHOD.into()))
                            && !self.function_ids.contains_key(&method_key(value.ty, crate::sema::DEBUG_METHOD))))) =>
            {
                let CoreExprKind::Field { value, .. } = &callee.kind else {
                    unreachable!()
                };
                self.lower_debug(value, function_names, types)
            }
            CoreExprKind::Call {
                callee, arguments, ..
            } if matches!(&callee.kind, CoreExprKind::Name(name) if name == "@dyn") => {
                self.lower_dynamic_value(expression, &arguments[0].value, function_names, types)
            }
            CoreExprKind::Call {
                callee, arguments, ..
            } if matches!(&callee.kind, CoreExprKind::Field { value, .. } if matches!(value.ty, Type::Dyn(_))) => {
                self.lower_dynamic_call(expression, callee, arguments, function_names, types)
            }
            CoreExprKind::Call {
                callee, arguments, ..
            } if matches!(&callee.kind, CoreExprKind::Field { value, access: FieldAccess::Name(method) }
                    if method == "new" && matches!(&value.kind, CoreExprKind::Name(name) if name == "CCallback")) =>
            {
                let mut operands = Vec::new();
                for (parameter, argument) in arguments.iter().enumerate() {
                    let Some(value) = self.lower_value(&argument.value, function_names, types)?
                    else {
                        return Ok(None);
                    };
                    operands.push(MirCallArgument { parameter, value });
                }
                let destination = self.next_value(expression.ty);
                let signature = self.value_types[operands[0].value.0];
                self.push_statement(MirStatement::RuntimeCall {
                    destination,
                    intrinsic: RuntimeIntrinsic::CCallbackNew(signature),
                    arguments: operands,
                });
                Ok(Some(destination))
            }
            CoreExprKind::Call { callee, .. } if matches!(&callee.kind, CoreExprKind::Field { value, .. } if value.ty == Type::CCallback) =>
            {
                let CoreExprKind::Field {
                    value: receiver,
                    access: FieldAccess::Name(method),
                } = &callee.kind
                else {
                    unreachable!()
                };
                let Some(value) = (if method == "close" {
                    self.lower_value(receiver, function_names, types)?
                } else {
                    self.lower_borrowed_value(receiver, function_names, types)?
                }) else {
                    return Ok(None);
                };
                let intrinsic = match method.as_str() {
                    "function" => RuntimeIntrinsic::CCallbackFunction,
                    "context" => RuntimeIntrinsic::CCallbackContext,
                    "failed" => RuntimeIntrinsic::CCallbackFailed,
                    "close" => RuntimeIntrinsic::CCallbackClose,
                    _ => return Err(Diagnostic::codegen("unknown CCallback method")),
                };
                let destination = self.next_value(expression.ty);
                self.push_statement(MirStatement::RuntimeCall {
                    destination,
                    intrinsic,
                    arguments: vec![MirCallArgument {
                        parameter: 0,
                        value,
                    }],
                });
                Ok(Some(destination))
            }
            CoreExprKind::Call { callee, .. }
                if expression.ty.is_c_pointer()
                    && matches!(
                        &callee.kind, CoreExprKind::Field { value, access: FieldAccess::Name(method) }
                        if method == "null" && matches!(&value.kind, CoreExprKind::Name(name)
                            if matches!(name.as_str(), "CPtr" | "CMutPtr" | "CStr"))
                    ) =>
            {
                let destination = self.next_value(expression.ty);
                self.push_statement(MirStatement::RuntimeCall {
                    destination,
                    intrinsic: RuntimeIntrinsic::CPointerNull(expression.ty),
                    arguments: vec![],
                });
                Ok(Some(destination))
            }
            CoreExprKind::Call { callee, .. }
                if matches!(
                    &callee.kind, CoreExprKind::Field { value, access: FieldAccess::Name(method) }
                    if value.ty.is_c_pointer() && method == "is_null"
                ) =>
            {
                let CoreExprKind::Field {
                    value: receiver, ..
                } = &callee.kind
                else {
                    unreachable!()
                };
                let Some(value) = self.lower_value(receiver, function_names, types)? else {
                    return Ok(None);
                };
                let destination = self.next_value(Type::Bool);
                self.push_statement(MirStatement::RuntimeCall {
                    destination,
                    intrinsic: RuntimeIntrinsic::CPointerIsNull,
                    arguments: vec![MirCallArgument {
                        parameter: 0,
                        value,
                    }],
                });
                Ok(Some(destination))
            }
            CoreExprKind::Call { callee, .. }
                if matches!(
                    &callee.kind, CoreExprKind::Field { value, access: FieldAccess::Name(method) }
                    if value.ty == Type::CStr && method == "to_string"
                ) =>
            {
                let CoreExprKind::Field {
                    value: receiver, ..
                } = &callee.kind
                else {
                    unreachable!()
                };
                let Some(value) = self.lower_value(receiver, function_names, types)? else {
                    return Ok(None);
                };
                let Type::Option(id) = expression.ty else {
                    return Err(Diagnostic::codegen(
                        "MIR CStr to_string has a non-Option type",
                    ));
                };
                let destination = self.next_value(Type::Option(id));
                self.push_statement(MirStatement::RuntimeCall {
                    destination,
                    intrinsic: RuntimeIntrinsic::CStrAsString,
                    arguments: vec![MirCallArgument {
                        parameter: 0,
                        value,
                    }],
                });
                Ok(Some(destination))
            }
            CoreExprKind::Call {
                callee, arguments, ..
            } if matches!(
                (&callee.kind, expression.ty),
                (CoreExprKind::Name(name), Type::Option(_)) if name == "Some"
            ) =>
            {
                let Type::Option(id) = expression.ty else {
                    unreachable!()
                };
                let Some(value) = self.lower_value(&arguments[0].value, function_names, types)?
                else {
                    return Ok(None);
                };
                let destination = self.next_value(expression.ty);
                self.push_statement(MirStatement::EnumConstruct {
                    destination,
                    enum_id: MirTypeId::Option(id),
                    variant: 0,
                    arguments: vec![MirCallArgument {
                        parameter: 0,
                        value,
                    }],
                });
                Ok(Some(destination))
            }
            CoreExprKind::Call {
                callee, arguments, ..
            } if matches!(
                (&callee.kind, expression.ty),
                (CoreExprKind::Name(name), Type::Result(_))
                    if matches!(name.as_str(), "Ok" | "Err")
            ) =>
            {
                let Type::Result(id) = expression.ty else {
                    unreachable!()
                };
                let CoreExprKind::Name(name) = &callee.kind else {
                    unreachable!()
                };
                let Some(value) = self.lower_value(&arguments[0].value, function_names, types)?
                else {
                    return Ok(None);
                };
                let destination = self.next_value(expression.ty);
                self.push_statement(MirStatement::EnumConstruct {
                    destination,
                    enum_id: MirTypeId::Result(id),
                    variant: usize::from(name == "Err"),
                    arguments: vec![MirCallArgument {
                        parameter: 0,
                        value,
                    }],
                });
                Ok(Some(destination))
            }
            CoreExprKind::Call {
                callee, arguments, ..
            } if matches!(
                &callee.kind,
                CoreExprKind::Field {
                    value,
                    access: FieldAccess::Name(method)
                } if matches!(&value.kind, CoreExprKind::Name(name) if name == "CMutPtr")
                    && method == "alloc"
            ) =>
            {
                let Some(content) = self.lower_value(&arguments[0].value, function_names, types)?
                else {
                    return Ok(None);
                };
                let content_type = self.value_types[content.0];
                let destination = self.next_value(expression.ty);
                self.push_statement(MirStatement::RuntimeCall {
                    destination,
                    intrinsic: RuntimeIntrinsic::CCellAlloc(content_type),
                    arguments: vec![MirCallArgument {
                        parameter: 0,
                        value: content,
                    }],
                });
                Ok(Some(destination))
            }
            CoreExprKind::Call {
                callee, arguments, ..
            } if matches!(
                &callee.kind,
                CoreExprKind::Field {
                    value,
                    access: FieldAccess::Name(method)
                } if matches!(value.ty, Type::CMutPtr(_))
                    && matches!(method.as_str(), "read" | "write" | "free")
            ) =>
            {
                let CoreExprKind::Field {
                    value: receiver,
                    access: FieldAccess::Name(method),
                    ..
                } = &callee.kind
                else {
                    unreachable!("cell method guard requires a named field");
                };
                let Type::CMutPtr(pointer_id) = receiver.ty else {
                    unreachable!("cell method guard requires a CMutPtr receiver");
                };
                let content = types.c_pointer_type(pointer_id);
                let Some(cell) = self.lower_value(receiver, function_names, types)? else {
                    return Ok(None);
                };
                let intrinsic = match method.as_str() {
                    "read" => RuntimeIntrinsic::CCellRead(content),
                    "write" => RuntimeIntrinsic::CCellWrite(content),
                    _ => RuntimeIntrinsic::CCellFree,
                };
                let mut mir_arguments = vec![MirCallArgument {
                    parameter: 0,
                    value: cell,
                }];
                let destination_type = if method == "read" {
                    content
                } else {
                    Type::Unit
                };
                if method == "write" {
                    let Some(value) =
                        self.lower_value(&arguments[0].value, function_names, types)?
                    else {
                        return Ok(None);
                    };
                    mir_arguments.push(MirCallArgument {
                        parameter: 1,
                        value,
                    });
                }
                let destination = self.next_value(destination_type);
                self.push_statement(MirStatement::RuntimeCall {
                    destination,
                    intrinsic,
                    arguments: mir_arguments,
                });
                Ok(Some(destination))
            }
            CoreExprKind::Call {
                callee, arguments, ..
            } if matches!(
                &callee.kind,
                CoreExprKind::Field {
                    value,
                    access: FieldAccess::Name(method)
                } if matches!(&value.kind, CoreExprKind::Name(name) if name == "Cown")
                    && method == "new"
            ) =>
            {
                self.lower_cown_new(expression, arguments, function_names, types)
            }
            CoreExprKind::Call {
                callee, arguments, ..
            } if matches!(
                &callee.kind,
                CoreExprKind::Field {
                    value,
                    access: FieldAccess::Name(method)
                } if matches!(&value.kind, CoreExprKind::Name(name) if name == "Bytes")
                    && method == "from_string"
            ) =>
            {
                self.lower_bytes_from_string(expression, arguments, function_names, types)
            }
            CoreExprKind::Call {
                callee, arguments, ..
            } if matches!(
                &callee.kind,
                CoreExprKind::Field {
                    value,
                    access: FieldAccess::Name(method)
                } if matches!(&value.kind, CoreExprKind::Name(name) if name == "MutBytes")
                    && matches!(method.as_str(), "with_capacity" | "from_bytes")
            ) =>
            {
                self.lower_mut_bytes_constructor(
                    expression,
                    callee,
                    arguments,
                    function_names,
                    types,
                )
            }
            CoreExprKind::Call {
                callee, arguments, ..
            } if matches!(
                (&callee.kind, expression.ty),
                (CoreExprKind::Name(name), Type::Struct(_) | Type::Class(_))
                    if !function_names.contains(name)
            ) =>
            {
                let CoreExprKind::Name(_type_name) = &callee.kind else {
                    unreachable!("constructor guard requires a named type");
                };
                let fields = self.lower_constructor_fields(
                    expression.ty,
                    arguments
                        .iter()
                        .map(|argument| (argument.label.as_deref(), &argument.value)),
                    function_names,
                    types,
                )?;
                let destination = self.next_value(expression.ty);
                self.push_statement(MirStatement::Construct {
                    destination,
                    type_id: MirTypeId::from_type(expression.ty).expect("struct/class type"),
                    fields,
                });
                Ok(Some(destination))
            }
            CoreExprKind::Call {
                callee, arguments, ..
            } if matches!(
                &callee.kind,
                CoreExprKind::Field {
                    value,
                    access: FieldAccess::Name(method)
                } if value.ty == Type::String && method == "as_cstr"
            ) =>
            {
                if !arguments.is_empty() {
                    return Err(Diagnostic::codegen(
                        "String as_cstr does not take arguments",
                    ));
                }
                self.lower_string_as_cstr(callee, function_names, types)
            }
            CoreExprKind::Call {
                callee,
                type_arguments,
                arguments,
                ..
            } if matches!(
                &callee.kind,
                CoreExprKind::Field {
                    value,
                    access: FieldAccess::Name(method)
                } if value.ty == Type::String && method == "parse"
            ) =>
            {
                self.lower_string_parse(
                    expression,
                    callee,
                    type_arguments,
                    arguments,
                    function_names,
                    types,
                )
            }
            CoreExprKind::Call {
                callee, arguments, ..
            } if matches!(
                &callee.kind,
                CoreExprKind::Field {
                    value,
                    access: FieldAccess::Name(method)
                } if value.ty == Type::String && method != crate::sema::SHOW_METHOD
            ) =>
            {
                self.lower_string_method(expression, callee, arguments, function_names, types)
            }
            CoreExprKind::Call {
                callee, arguments, ..
            } if !matches!(callee.ty, Type::Function(_))
                && matches!(
                    &callee.kind,
                    CoreExprKind::Field {
                        value,
                        access: FieldAccess::Name(_)
                    } if matches!(value.ty, Type::Class(_) | Type::Struct(_))
                ) =>
            {
                let CoreExprKind::Field {
                    value: receiver,
                    access: FieldAccess::Name(method),
                } = &callee.kind
                else {
                    unreachable!("method call guard requires a named field");
                };
                let receiver_mode = self
                    .function_bodies
                    .get(&method_key(receiver.ty, method))
                    .map(|function| function.receiver_mode)
                    .ok_or_else(|| {
                        Diagnostic::codegen(format!(
                            "MIR method body for {:?}.{method} was not resolved",
                            receiver.ty
                        ))
                    })?;
                let Some(receiver_value) = (if receiver_mode == crate::syntax::ReceiverMode::Owned {
                    self.lower_value(receiver, function_names, types)?
                } else {
                    self.lower_borrowed_value(receiver, function_names, types)?
                }) else {
                    return Ok(None);
                };
                let method_id = *self
                    .function_ids
                    .get(&method_key(receiver.ty, method))
                    .ok_or_else(|| Diagnostic::codegen("MIR method was not resolved"))?;
                let parameter_names = self
                    .function_parameters
                    .get(&method_id)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                let mut used_parameters = vec![false; parameter_names.len()];
                let mut next_positional = 0;
                let mut mir_arguments = Vec::with_capacity(arguments.len());
                for argument in arguments {
                    let parameter = resolve_argument_index(
                        argument.label.as_deref(),
                        parameter_names,
                        &mut used_parameters,
                        &mut next_positional,
                    )?;
                    let borrowed = self
                        .function_bodies
                        .get(&method_key(receiver.ty, method))
                        .and_then(|body| body.parameter_ownership.get(parameter))
                        .is_some_and(|mode| {
                            *mode == Some(crate::hir::CoreParameterOwnership::Borrowed)
                        });
                    let Some(value) = (if borrowed {
                        self.lower_borrowed_value(&argument.value, function_names, types)?
                    } else {
                        self.lower_value(&argument.value, function_names, types)?
                    }) else {
                        return Ok(None);
                    };
                    mir_arguments.push(MirCallArgument { parameter, value });
                }
                self.lower_method_call_values(
                    receiver_value,
                    method,
                    expression.ty,
                    mir_arguments,
                    function_names,
                    types,
                )
            }
            CoreExprKind::Call { callee, .. } if is_collection_call(callee, expression.ty) => {
                self.lower_collection_call(expression, function_names, types)
            }
            CoreExprKind::Call {
                callee, arguments, ..
            } if matches!(
                &callee.kind,
                CoreExprKind::Field {
                    value: _,
                    access: FieldAccess::Name(method)
                } if method == crate::sema::SHOW_METHOD
            ) =>
            {
                let CoreExprKind::Field {
                    value: receiver, ..
                } = &callee.kind
                else {
                    unreachable!()
                };
                if types.has_show_impl(receiver.ty) {
                    let Some(receiver_value) =
                        self.lower_borrowed_value(receiver, function_names, types)?
                    else {
                        return Ok(None);
                    };
                    let method_id = *self
                        .function_ids
                        .get(&method_key(receiver.ty, crate::sema::SHOW_METHOD))
                        .ok_or_else(|| Diagnostic::codegen("MIR Show method was not resolved"))?;
                    let destination = self.next_value(Type::String);
                    self.push_statement(MirStatement::MethodCall {
                        destination,
                        receiver: receiver_value,
                        receiver_type: receiver.ty,
                        method: method_id,
                        arguments: Vec::new(),
                        continuation: None, // Phase 2: will be added in post-processing
                    });
                    return Ok(Some(destination));
                }
                let Some(value) = self.lower_value(receiver, function_names, types)? else {
                    return Ok(None);
                };
                let destination = self.next_value(Type::String);
                self.push_statement(MirStatement::RuntimeCall {
                    destination,
                    intrinsic: RuntimeIntrinsic::Show(receiver.ty),
                    arguments: vec![MirCallArgument {
                        parameter: 0,
                        value,
                    }],
                });
                Ok(Some(destination))
            }
            CoreExprKind::Call {
                callee, arguments, ..
            } if matches!(
                &callee.kind,
                CoreExprKind::Name(name) if matches!(name.as_str(), "print" | "println")
            ) =>
            {
                self.lower_print(callee, arguments, function_names, types)
            }
            CoreExprKind::Call {
                callee, arguments, ..
            } if matches!(
                &callee.kind,
                CoreExprKind::Name(name) if name == "panic"
            ) =>
            {
                self.lower_panic(arguments, function_names, types)
            }
            CoreExprKind::Call {
                callee, arguments, ..
            } if matches!(
                (&callee.kind, expression.ty),
                (
                    CoreExprKind::Field {
                        value,
                        access: FieldAccess::Name(_),
                    },
                    Type::Enum(_)
                ) if matches!(
                    &value.kind,
                    CoreExprKind::Name(name) if types.enum_type(name).is_some()
                )
            ) =>
            {
                let CoreExprKind::Field {
                    value,
                    access: FieldAccess::Name(variant),
                } = &callee.kind
                else {
                    unreachable!("enum constructor guard requires a named variant");
                };
                let CoreExprKind::Name(_enum_name) = &value.kind else {
                    unreachable!("enum constructors require a named enum");
                };
                let variant_index = types
                    .enum_variants(match expression.ty {
                        Type::Enum(id) => id,
                        _ => unreachable!(),
                    })
                    .iter()
                    .position(|candidate| candidate.name == *variant)
                    .expect("enum variant resolved");
                let enum_parameter_names = types.enum_variants(match expression.ty {
                    Type::Enum(id) => id,
                    _ => unreachable!(),
                })[variant_index]
                    .fields
                    .iter()
                    .map(|(name, _)| name.clone())
                    .collect::<Vec<_>>();
                let mut used_parameters = vec![false; enum_parameter_names.len()];
                let mut next_positional = 0;
                let mut mir_arguments = Vec::with_capacity(arguments.len());
                for argument in arguments {
                    let Some(value) = self.lower_value(&argument.value, function_names, types)?
                    else {
                        return Ok(None);
                    };
                    mir_arguments.push(MirCallArgument {
                        parameter: resolve_argument_index(
                            argument.label.as_deref(),
                            &enum_parameter_names,
                            &mut used_parameters,
                            &mut next_positional,
                        )?,
                        value,
                    });
                }
                let destination = self.next_value(expression.ty);
                self.push_statement(MirStatement::EnumConstruct {
                    destination,
                    enum_id: MirTypeId::from_type(expression.ty).expect("enum type"),
                    variant: variant_index,
                    arguments: mir_arguments,
                });
                Ok(Some(destination))
            }
            CoreExprKind::Call {
                callee,
                effect_operation: Some(operation),
                arguments,
                ..
            } => self.lower_effect_call(
                expression,
                callee,
                *operation,
                arguments,
                function_names,
                types,
            ),
            CoreExprKind::Call {
                callee, arguments, ..
            } if matches!(&callee.kind, CoreExprKind::ExternalSymbol(_)) => {
                let CoreExprKind::ExternalSymbol(symbol) = &callee.kind else {
                    unreachable!();
                };
                self.lower_external_call(expression, symbol, arguments, function_names, types)
            }
            CoreExprKind::Call {
                callee, arguments, ..
            } if matches!(callee.ty, Type::Function(_)) => {
                self.lower_function_call(expression, callee, arguments, function_names, types)
            }
            CoreExprKind::Call {
                callee, arguments, ..
            } if matches!(
                &callee.kind,
                CoreExprKind::Name(name)
                    if !matches!(name.as_str(), "print" | "println")
                        && function_names.contains(name)
            ) =>
            {
                let CoreExprKind::Name(function) = &callee.kind else {
                    unreachable!("the call guard requires a named function");
                };
                self.lower_named_call(expression, function, arguments, function_names, types)
            }
            _ => Err(Diagnostic::codegen(format!(
                "unsupported call expression in MIR lowering: {:?}",
                expression.kind
            ))),
        }
    }
}

fn compares_option_with_none(
    callee: &CoreExpr,
    arguments: &[crate::hir::CoreCallArgument],
) -> bool {
    let CoreExprKind::Field {
        value,
        access: FieldAccess::Name(method),
    } = &callee.kind
    else {
        return false;
    };
    method == crate::sema::PARTIAL_EQ_METHOD
        && matches!(value.ty, Type::Option(_))
        && arguments.len() == 1
        && (super::equality::is_option_none(value)
            || super::equality::is_option_none(&arguments[0].value))
}
