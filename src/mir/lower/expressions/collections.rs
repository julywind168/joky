use super::super::*;
use crate::hir::CoreCollectionLiteral;

impl Lowerer<'_> {
    pub(super) fn lower_collection_literal(
        &mut self,
        literal: &CoreCollectionLiteral,
        expression_type: Type,
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        match literal {
            CoreCollectionLiteral::List(elements) => {
                let Type::List(list_id) = expression_type else {
                    return Err(Diagnostic::codegen("List literal type was not resolved"));
                };
                let mut current = self.next_value(expression_type);
                self.push_statement(MirStatement::RuntimeCall {
                    destination: current,
                    intrinsic: RuntimeIntrinsic::ListEmpty(types.list_type(list_id)),
                    arguments: Vec::new(),
                });
                for element in elements.iter().rev() {
                    let Some(head) = self.lower_value(element, function_names, types)? else {
                        return Ok(None);
                    };
                    let destination = self.next_value(expression_type);
                    self.push_statement(MirStatement::RuntimeCall {
                        destination,
                        intrinsic: RuntimeIntrinsic::ListCons(types.list_type(list_id)),
                        arguments: vec![
                            MirCallArgument {
                                parameter: 0,
                                value: head,
                            },
                            MirCallArgument {
                                parameter: 1,
                                value: current,
                            },
                        ],
                    });
                    current = destination;
                }
                Ok(Some(current))
            }
            CoreCollectionLiteral::MutList(elements) => {
                let Type::MutList(list_id) = expression_type else {
                    return Err(Diagnostic::codegen("MutList literal type was not resolved"));
                };
                let current = self.next_value(expression_type);
                self.push_statement(MirStatement::RuntimeCall {
                    destination: current,
                    intrinsic: RuntimeIntrinsic::MutListNew(types.list_type(list_id)),
                    arguments: Vec::new(),
                });
                let local = self.new_local("<mut-list-literal>", expression_type);
                let bound = self.next_value(Type::Unit);
                self.push_statement(MirStatement::Bind {
                    local,
                    value: Some(current),
                    destination: bound,
                });
                for element in elements {
                    let Some(item) = self.lower_value(element, function_names, types)? else {
                        return Ok(None);
                    };
                    let receiver =
                        self.next_value_with_ownership(expression_type, MirOwnership::Borrowed);
                    self.push_statement(MirStatement::BorrowLocal {
                        destination: receiver,
                        local,
                    });
                    let destination = self.next_value(Type::Unit);
                    self.push_statement(MirStatement::RuntimeCall {
                        destination,
                        intrinsic: RuntimeIntrinsic::MutListPush(types.list_type(list_id)),
                        arguments: vec![
                            MirCallArgument {
                                parameter: 0,
                                value: receiver,
                            },
                            MirCallArgument {
                                parameter: 1,
                                value: item,
                            },
                        ],
                    });
                }
                let result = self.next_value(expression_type);
                self.push_statement(MirStatement::TakeLocal {
                    destination: result,
                    local,
                });
                Ok(Some(result))
            }
            CoreCollectionLiteral::Set(elements) => {
                let Type::Map(map_id) = expression_type else {
                    return Err(Diagnostic::codegen("Set literal type was not resolved"));
                };
                let info = types.map_info(map_id);
                let mut current = self.next_value(expression_type);
                self.push_statement(MirStatement::RuntimeCall {
                    destination: current,
                    intrinsic: RuntimeIntrinsic::MapEmpty(info.key, info.value),
                    arguments: Vec::new(),
                });
                for element in elements {
                    let Some(key) = self.lower_value(element, function_names, types)? else {
                        return Ok(None);
                    };
                    let value = self.next_value(Type::Bool);
                    self.push_statement(MirStatement::Const {
                        destination: value,
                        value: MirConstant::Boolean(true),
                    });
                    let destination = self.next_value(expression_type);
                    self.push_statement(MirStatement::RuntimeCall {
                        destination,
                        intrinsic: RuntimeIntrinsic::MapInsert(info.key, info.value),
                        arguments: vec![
                            MirCallArgument {
                                parameter: 0,
                                value: current,
                            },
                            MirCallArgument {
                                parameter: 1,
                                value: key,
                            },
                            MirCallArgument {
                                parameter: 2,
                                value,
                            },
                        ],
                    });
                    current = destination;
                }
                Ok(Some(current))
            }
            CoreCollectionLiteral::MutSet(elements) => {
                let Type::MutSet(set_id) = expression_type else {
                    return Err(Diagnostic::codegen("MutSet literal type was not resolved"));
                };
                let info = types.map_info(set_id);
                let current = self.next_value(expression_type);
                self.push_statement(MirStatement::RuntimeCall {
                    destination: current,
                    intrinsic: RuntimeIntrinsic::MutSetNew(info.key),
                    arguments: Vec::new(),
                });
                let local = self.new_local("<mut-set-literal>", expression_type);
                let bound = self.next_value(Type::Unit);
                self.push_statement(MirStatement::Bind {
                    local,
                    value: Some(current),
                    destination: bound,
                });
                for element in elements {
                    let Some(key) = self.lower_value(element, function_names, types)? else {
                        return Ok(None);
                    };
                    let receiver =
                        self.next_value_with_ownership(expression_type, MirOwnership::Borrowed);
                    self.push_statement(MirStatement::BorrowLocal {
                        destination: receiver,
                        local,
                    });
                    let destination = self.next_value(Type::Bool);
                    self.push_statement(MirStatement::RuntimeCall {
                        destination,
                        intrinsic: RuntimeIntrinsic::MutSetAdd(info.key),
                        arguments: vec![
                            MirCallArgument {
                                parameter: 0,
                                value: receiver,
                            },
                            MirCallArgument {
                                parameter: 1,
                                value: key,
                            },
                        ],
                    });
                }
                let result = self.next_value(expression_type);
                self.push_statement(MirStatement::TakeLocal {
                    destination: result,
                    local,
                });
                Ok(Some(result))
            }
            CoreCollectionLiteral::Map(entries) => {
                let Type::Map(map_id) = expression_type else {
                    return Err(Diagnostic::codegen("Map literal type was not resolved"));
                };
                let info = types.map_info(map_id);
                let mut current = self.next_value(expression_type);
                self.push_statement(MirStatement::RuntimeCall {
                    destination: current,
                    intrinsic: RuntimeIntrinsic::MapEmpty(info.key, info.value),
                    arguments: Vec::new(),
                });
                for entry in entries {
                    let Some(key) = self.lower_value(&entry.key, function_names, types)? else {
                        return Ok(None);
                    };
                    let Some(value) = self.lower_value(&entry.value, function_names, types)? else {
                        return Ok(None);
                    };
                    let destination = self.next_value(expression_type);
                    self.push_statement(MirStatement::RuntimeCall {
                        destination,
                        intrinsic: RuntimeIntrinsic::MapInsert(info.key, info.value),
                        arguments: vec![
                            MirCallArgument {
                                parameter: 0,
                                value: current,
                            },
                            MirCallArgument {
                                parameter: 1,
                                value: key,
                            },
                            MirCallArgument {
                                parameter: 2,
                                value,
                            },
                        ],
                    });
                    current = destination;
                }
                Ok(Some(current))
            }
            CoreCollectionLiteral::MutMap(entries) => {
                let Type::MutMap(map_id) = expression_type else {
                    return Err(Diagnostic::codegen("MutMap literal type was not resolved"));
                };
                let info = types.map_info(map_id);
                let current = self.next_value(expression_type);
                self.push_statement(MirStatement::RuntimeCall {
                    destination: current,
                    intrinsic: RuntimeIntrinsic::MutMapNew(info.key, info.value),
                    arguments: Vec::new(),
                });
                let local = self.new_local("<mut-map-literal>", expression_type);
                let bound = self.next_value(Type::Unit);
                self.push_statement(MirStatement::Bind {
                    local,
                    value: Some(current),
                    destination: bound,
                });
                for entry in entries {
                    let Some(key) = self.lower_value(&entry.key, function_names, types)? else {
                        return Ok(None);
                    };
                    let Some(value) = self.lower_value(&entry.value, function_names, types)? else {
                        return Ok(None);
                    };
                    let receiver =
                        self.next_value_with_ownership(expression_type, MirOwnership::Borrowed);
                    self.push_statement(MirStatement::BorrowLocal {
                        destination: receiver,
                        local,
                    });
                    let destination =
                        self.next_value(Type::Option(types.option_id(info.value).ok_or_else(
                            || Diagnostic::codegen("MutMap literal Option type was not interned"),
                        )?));
                    self.push_statement(MirStatement::RuntimeCall {
                        destination,
                        intrinsic: RuntimeIntrinsic::MutMapInsert(info.key, info.value),
                        arguments: vec![
                            MirCallArgument {
                                parameter: 0,
                                value: receiver,
                            },
                            MirCallArgument {
                                parameter: 1,
                                value: key,
                            },
                            MirCallArgument {
                                parameter: 2,
                                value,
                            },
                        ],
                    });
                    self.discard_value(Some(destination));
                }
                let result = self.next_value(expression_type);
                self.push_statement(MirStatement::TakeLocal {
                    destination: result,
                    local,
                });
                Ok(Some(result))
            }
        }
    }
}

pub(super) fn is_collection_call(callee: &CoreExpr, expression_type: Type) -> bool {
    match &callee.kind {
        CoreExprKind::Name(name) => matches!(
            name.as_str(),
            "List" | "Bytes" | "MutBytes" | "MutList" | "MutMap" | "MutSet"
        ),
        CoreExprKind::Field {
            value,
            access: FieldAccess::Name(method),
        } => {
            matches!(
                value.ty,
                Type::List(_)
                    | Type::Bytes
                    | Type::MutBytes
                    | Type::MutList(_)
                    | Type::Map(_)
                    | Type::MutMap(_)
                    | Type::MutSet(_)
            ) || (method == "empty"
                && match (&value.kind, expression_type) {
                    (CoreExprKind::Name(name), Type::List(_)) if name == "List" => true,
                    (CoreExprKind::Name(name), Type::Map(_)) if name == "Map" || name == "Set" => {
                        true
                    }
                    (CoreExprKind::Name(name), Type::MutMap(_))
                        if name == "MutMap" && method == "empty" =>
                    {
                        true
                    }
                    (CoreExprKind::Name(name), Type::MutSet(_))
                        if name == "MutSet" && method == "empty" =>
                    {
                        true
                    }
                    _ => false,
                })
        }
        _ => false,
    }
}

impl Lowerer<'_> {
    pub(super) fn lower_collection_call(
        &mut self,
        expression: &CoreExpr,
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        match &expression.kind {
            CoreExprKind::Call {
                callee, arguments, ..
            } if matches!(&callee.kind, CoreExprKind::Name(name) if name == "List") => {
                let Type::List(list_id) = expression.ty else {
                    return Err(Diagnostic::codegen(
                        "List constructor type was not resolved",
                    ));
                };
                let mut current = self.next_value(expression.ty);
                self.push_statement(MirStatement::RuntimeCall {
                    destination: current,
                    intrinsic: RuntimeIntrinsic::ListEmpty(types.list_type(list_id)),
                    arguments: Vec::new(),
                });
                for argument in arguments.iter().rev() {
                    let Some(head) = self.lower_value(&argument.value, function_names, types)?
                    else {
                        return Ok(None);
                    };
                    let destination = self.next_value(expression.ty);
                    self.push_statement(MirStatement::RuntimeCall {
                        destination,
                        intrinsic: RuntimeIntrinsic::ListCons(types.list_type(list_id)),
                        arguments: vec![
                            MirCallArgument {
                                parameter: 0,
                                value: head,
                            },
                            MirCallArgument {
                                parameter: 1,
                                value: current,
                            },
                        ],
                    });
                    current = destination;
                }
                Ok(Some(current))
            }
            CoreExprKind::Call {
                callee, arguments, ..
            } if matches!(&callee.kind, CoreExprKind::Name(name) if name == "Bytes") => {
                if !arguments.is_empty() {
                    return Err(Diagnostic::codegen(
                        "Bytes constructor arguments were not resolved",
                    ));
                }
                let destination = self.next_value(expression.ty);
                self.push_statement(MirStatement::RuntimeCall {
                    destination,
                    intrinsic: RuntimeIntrinsic::BytesNew,
                    arguments: Vec::new(),
                });
                Ok(Some(destination))
            }
            CoreExprKind::Call {
                callee, arguments, ..
            } if matches!(&callee.kind, CoreExprKind::Name(name) if name == "MutBytes") => {
                if !arguments.is_empty() {
                    return Err(Diagnostic::codegen(
                        "MutBytes constructor arguments were not resolved",
                    ));
                }
                let destination = self.next_value(expression.ty);
                self.push_statement(MirStatement::RuntimeCall {
                    destination,
                    intrinsic: RuntimeIntrinsic::MutBytesNew,
                    arguments: Vec::new(),
                });
                Ok(Some(destination))
            }
            CoreExprKind::Call {
                callee,
                type_arguments,
                arguments: constructor_arguments,
                ..
            } if matches!(&callee.kind, CoreExprKind::Name(name) if name == "MutList") => {
                let Type::MutList(id) = expression.ty else {
                    return Err(Diagnostic::codegen(
                        "MutList constructor type was not resolved",
                    ));
                };
                if !type_arguments.is_empty() && constructor_arguments.is_empty() {
                    let destination = self.next_value(expression.ty);
                    self.push_statement(MirStatement::RuntimeCall {
                        destination,
                        intrinsic: RuntimeIntrinsic::MutListNew(types.list_type(id)),
                        arguments: Vec::new(),
                    });
                    return Ok(Some(destination));
                }
                Err(Diagnostic::codegen(
                    "MutList constructor arguments were not resolved",
                ))
            }
            CoreExprKind::Call {
                callee,
                type_arguments,
                arguments: constructor_arguments,
                ..
            } if matches!(&callee.kind, CoreExprKind::Name(name) if name == "MutMap") => {
                let Type::MutMap(id) = expression.ty else {
                    return Err(Diagnostic::codegen(
                        "MutMap constructor type was not resolved",
                    ));
                };
                if !type_arguments.is_empty() && constructor_arguments.is_empty() {
                    let info = types.map_info(id);
                    let destination = self.next_value(expression.ty);
                    self.push_statement(MirStatement::RuntimeCall {
                        destination,
                        intrinsic: RuntimeIntrinsic::MutMapNew(info.key, info.value),
                        arguments: Vec::new(),
                    });
                    return Ok(Some(destination));
                }
                Err(Diagnostic::codegen(
                    "MutMap constructor arguments were not resolved",
                ))
            }
            CoreExprKind::Call {
                callee,
                type_arguments,
                arguments: constructor_arguments,
                ..
            } if matches!(&callee.kind, CoreExprKind::Name(name) if name == "MutSet") => {
                let Type::MutSet(id) = expression.ty else {
                    return Err(Diagnostic::codegen(
                        "MutSet constructor type was not resolved",
                    ));
                };
                if !type_arguments.is_empty() && constructor_arguments.is_empty() {
                    let info = types.map_info(id);
                    let destination = self.next_value(expression.ty);
                    self.push_statement(MirStatement::RuntimeCall {
                        destination,
                        intrinsic: RuntimeIntrinsic::MutSetNew(info.key),
                        arguments: Vec::new(),
                    });
                    return Ok(Some(destination));
                }
                Err(Diagnostic::codegen(
                    "MutSet constructor arguments were not resolved",
                ))
            }
            CoreExprKind::Call {
                callee,
                type_arguments,
                arguments,
                ..
            } if matches!(&callee.kind, CoreExprKind::Field { value, access: FieldAccess::Name(method) } if matches!(value.kind, CoreExprKind::Name(ref name) if name == "List") && method == "empty") =>
            {
                let Type::List(list_id) = expression.ty else {
                    unreachable!()
                };
                if !arguments.is_empty() || type_arguments.len() != 1 {
                    return Err(Diagnostic::codegen(
                        "List.empty arguments were not resolved",
                    ));
                }
                let destination = self.next_value(expression.ty);
                self.push_statement(MirStatement::RuntimeCall {
                    destination,
                    intrinsic: RuntimeIntrinsic::ListEmpty(types.list_type(list_id)),
                    arguments: Vec::new(),
                });
                Ok(Some(destination))
            }
            CoreExprKind::Call {
                callee, arguments, ..
            } if matches!(&callee.kind, CoreExprKind::Field { value, access: FieldAccess::Name(method) } if matches!(value.kind, CoreExprKind::Name(ref name) if (name == "Map" || name == "Set") && method == "empty") ) =>
            {
                let Type::Map(map_id) = expression.ty else {
                    unreachable!()
                };
                if !arguments.is_empty() {
                    return Err(Diagnostic::codegen(
                        "Map/Set.empty arguments were not resolved",
                    ));
                }
                let info = types.map_info(map_id);
                let destination = self.next_value(expression.ty);
                self.push_statement(MirStatement::RuntimeCall {
                    destination,
                    intrinsic: RuntimeIntrinsic::MapEmpty(info.key, info.value),
                    arguments: Vec::new(),
                });
                Ok(Some(destination))
            }
            CoreExprKind::Call {
                callee, arguments, ..
            } if matches!(&callee.kind, CoreExprKind::Field { value, access: FieldAccess::Name(method) } if matches!(value.kind, CoreExprKind::Name(ref name) if matches!(name.as_str(), "MutMap" | "MutSet") && method == "empty") ) =>
            {
                if !arguments.is_empty() {
                    return Err(Diagnostic::codegen(
                        "MutMap/MutSet.empty arguments were not resolved",
                    ));
                }
                let (intrinsic, expression_type) = match expression.ty {
                    Type::MutMap(map_id) => {
                        let info = types.map_info(map_id);
                        (
                            RuntimeIntrinsic::MutMapNew(info.key, info.value),
                            expression.ty,
                        )
                    }
                    Type::MutSet(map_id) => {
                        let info = types.map_info(map_id);
                        (RuntimeIntrinsic::MutSetNew(info.key), expression.ty)
                    }
                    _ => {
                        return Err(Diagnostic::codegen(
                            "MutMap/MutSet.empty has an invalid result type",
                        ))
                    }
                };
                let destination = self.next_value(expression_type);
                self.push_statement(MirStatement::RuntimeCall {
                    destination,
                    intrinsic,
                    arguments: Vec::new(),
                });
                Ok(Some(destination))
            }
            CoreExprKind::Call {
                callee, arguments, ..
            } if matches!(&callee.kind, CoreExprKind::Field { value, access: FieldAccess::Name(_) } if matches!(value.ty, Type::List(_))) =>
            {
                let CoreExprKind::Field {
                    value,
                    access: FieldAccess::Name(method),
                } = &callee.kind
                else {
                    unreachable!()
                };
                let Type::List(list_id) = value.ty else {
                    unreachable!()
                };
                let Some(list) = self.lower_value(value, function_names, types)? else {
                    return Ok(None);
                };
                let intrinsic = match method.as_str() {
                    "is_empty" => RuntimeIntrinsic::ListIsEmpty,
                    "head" => RuntimeIntrinsic::ListHead(types.list_type(list_id)),
                    "tail" => RuntimeIntrinsic::ListTail(types.list_type(list_id)),
                    "length" => RuntimeIntrinsic::ListLength,
                    "reverse" => RuntimeIntrinsic::ListReverse(types.list_type(list_id)),
                    "push_front" => RuntimeIntrinsic::ListCons(types.list_type(list_id)),
                    _ => return Err(Diagnostic::codegen("MIR List method was not resolved")),
                };
                let destination = self.next_value(expression.ty);
                let mut values = Vec::with_capacity(arguments.len() + 1);
                if method == "push_front" {
                    let Some(head) =
                        self.lower_value(&arguments[0].value, function_names, types)?
                    else {
                        return Ok(None);
                    };
                    values.push(MirCallArgument {
                        parameter: 0,
                        value: head,
                    });
                    values.push(MirCallArgument {
                        parameter: 1,
                        value: list,
                    });
                } else {
                    values.push(MirCallArgument {
                        parameter: 0,
                        value: list,
                    });
                }
                self.push_statement(MirStatement::RuntimeCall {
                    destination,
                    intrinsic,
                    arguments: values,
                });
                Ok(Some(destination))
            }
            CoreExprKind::Call {
                callee, arguments, ..
            } if matches!(&callee.kind, CoreExprKind::Field { value, access: FieldAccess::Name(_) } if value.ty == Type::Bytes) =>
            {
                let CoreExprKind::Field {
                    value: receiver,
                    access: FieldAccess::Name(method),
                } = &callee.kind
                else {
                    unreachable!()
                };
                let Some(receiver) = self.lower_value(receiver, function_names, types)? else {
                    return Ok(None);
                };
                let intrinsic = match method.as_str() {
                    "is_empty" => RuntimeIntrinsic::BytesIsEmpty,
                    "length" => RuntimeIntrinsic::BytesLength,
                    "get" => RuntimeIntrinsic::BytesGet,
                    "slice" => RuntimeIntrinsic::BytesSlice,
                    "concat" => RuntimeIntrinsic::BytesConcat,
                    "to_string" => RuntimeIntrinsic::BytesToString,
                    "iter" => RuntimeIntrinsic::BytesCursorNew,
                    _ => return Err(Diagnostic::codegen("MIR Bytes method was not resolved")),
                };
                let destination = self.next_value(expression.ty);
                let mut values = vec![MirCallArgument {
                    parameter: 0,
                    value: receiver,
                }];
                for (index, argument) in arguments.iter().enumerate() {
                    let Some(value) = self.lower_value(&argument.value, function_names, types)?
                    else {
                        return Ok(None);
                    };
                    values.push(MirCallArgument {
                        parameter: index + 1,
                        value,
                    });
                }
                self.push_statement(MirStatement::RuntimeCall {
                    destination,
                    intrinsic,
                    arguments: values,
                });
                Ok(Some(destination))
            }
            CoreExprKind::Call {
                callee, arguments, ..
            } if matches!(&callee.kind, CoreExprKind::Field { value, access: FieldAccess::Name(_) } if value.ty == Type::MutBytes) =>
            {
                let CoreExprKind::Field {
                    value: receiver,
                    access: FieldAccess::Name(method),
                } = &callee.kind
                else {
                    unreachable!()
                };
                let Some(receiver) = self.lower_borrowed_value(receiver, function_names, types)?
                else {
                    return Ok(None);
                };
                let intrinsic = match method.as_str() {
                    "length" => RuntimeIntrinsic::MutBytesLength,
                    "capacity" => RuntimeIntrinsic::MutBytesCapacity,
                    "push" => RuntimeIntrinsic::MutBytesPush,
                    "get" => RuntimeIntrinsic::MutBytesGet,
                    "set" => RuntimeIntrinsic::MutBytesSet,
                    "pop" => RuntimeIntrinsic::MutBytesPop,
                    "clear" => RuntimeIntrinsic::MutBytesClear,
                    "reserve" => RuntimeIntrinsic::MutBytesReserve,
                    "to_bytes" => RuntimeIntrinsic::MutBytesToBytes,
                    "extend" => RuntimeIntrinsic::MutBytesExtend,
                    _ => return Err(Diagnostic::codegen("MIR MutBytes method was not resolved")),
                };
                let destination = self.next_value(expression.ty);
                let mut values = vec![MirCallArgument {
                    parameter: 0,
                    value: receiver,
                }];
                for (index, argument) in arguments.iter().enumerate() {
                    let Some(value) = self.lower_value(&argument.value, function_names, types)?
                    else {
                        return Ok(None);
                    };
                    values.push(MirCallArgument {
                        parameter: index + 1,
                        value,
                    });
                }
                self.push_statement(MirStatement::RuntimeCall {
                    destination,
                    intrinsic,
                    arguments: values,
                });
                Ok(Some(destination))
            }
            CoreExprKind::Call {
                callee, arguments, ..
            } if matches!(&callee.kind, CoreExprKind::Field { value, access: FieldAccess::Name(_) } if matches!(value.ty, Type::MutList(_))) =>
            {
                let CoreExprKind::Field {
                    value: receiver,
                    access: FieldAccess::Name(method),
                } = &callee.kind
                else {
                    unreachable!()
                };
                let Type::MutList(id) = receiver.ty else {
                    unreachable!()
                };
                let Some(list) = (if method == "into_iter" {
                    self.lower_value(receiver, function_names, types)?
                } else {
                    self.lower_borrowed_value(receiver, function_names, types)?
                }) else {
                    return Ok(None);
                };
                let intrinsic = match method.as_str() {
                    "push" => RuntimeIntrinsic::MutListPush(types.list_type(id)),
                    "get" => RuntimeIntrinsic::MutListGet(types.list_type(id)),
                    "set" => RuntimeIntrinsic::MutListSet(types.list_type(id)),
                    "pop" => RuntimeIntrinsic::MutListPop(types.list_type(id)),
                    "length" => RuntimeIntrinsic::MutListLength,
                    "capacity" => RuntimeIntrinsic::MutListCapacity,
                    "into_iter" => RuntimeIntrinsic::MutListIntoIter(types.list_type(id)),
                    "to_list" => RuntimeIntrinsic::MutListToList(types.list_type(id)),
                    _ => return Err(Diagnostic::codegen("MIR MutList method was not resolved")),
                };
                let destination = self.next_value(expression.ty);
                let mut values = vec![MirCallArgument {
                    parameter: 0,
                    value: list,
                }];
                for (index, argument) in arguments.iter().enumerate() {
                    let Some(value) = self.lower_value(&argument.value, function_names, types)?
                    else {
                        return Ok(None);
                    };
                    values.push(MirCallArgument {
                        parameter: index + 1,
                        value,
                    });
                }
                self.push_statement(MirStatement::RuntimeCall {
                    destination,
                    intrinsic,
                    arguments: values,
                });
                Ok(Some(destination))
            }
            CoreExprKind::Call {
                callee, arguments, ..
            } if matches!(&callee.kind, CoreExprKind::Field { value, access: FieldAccess::Name(_) } if matches!(value.ty, Type::Map(_))) =>
            {
                let CoreExprKind::Field {
                    value,
                    access: FieldAccess::Name(method),
                } = &callee.kind
                else {
                    unreachable!()
                };
                let Type::Map(map_id) = value.ty else {
                    unreachable!()
                };
                let info = types.map_info(map_id);
                let Some(map) = self.lower_value(value, function_names, types)? else {
                    return Ok(None);
                };
                let intrinsic = match method.as_str() {
                    "insert" => RuntimeIntrinsic::MapInsert(info.key, info.value),
                    "get" => RuntimeIntrinsic::MapGet(info.key, info.value),
                    "remove" => RuntimeIntrinsic::MapRemove(info.key, info.value),
                    "contains_key" => RuntimeIntrinsic::MapContainsKey(info.key),
                    "length" => RuntimeIntrinsic::MapLength,
                    "is_empty" => RuntimeIntrinsic::MapIsEmpty,
                    "entries" => RuntimeIntrinsic::MapEntriesCursorNew(info.key, info.value),
                    "keys" | "iter" => RuntimeIntrinsic::MapKeysCursorNew(info.key, info.value),
                    "values" => RuntimeIntrinsic::MapValuesCursorNew(info.key, info.value),
                    _ => return Err(Diagnostic::codegen("MIR Map method was not resolved")),
                };
                let destination = self.next_value(expression.ty);
                let mut values = Vec::with_capacity(arguments.len() + 1);
                values.push(MirCallArgument {
                    parameter: 0,
                    value: map,
                });
                for (index, argument) in arguments.iter().enumerate() {
                    let Some(argument) =
                        self.lower_value(&argument.value, function_names, types)?
                    else {
                        return Ok(None);
                    };
                    values.push(MirCallArgument {
                        parameter: index + 1,
                        value: argument,
                    });
                }
                // Cursor-producing methods take no runtime arguments beyond the
                // borrowed map; codegen releases that reference after the call.
                self.push_statement(MirStatement::RuntimeCall {
                    destination,
                    intrinsic,
                    arguments: values,
                });
                Ok(Some(destination))
            }
            CoreExprKind::Call {
                callee, arguments, ..
            } if matches!(&callee.kind, CoreExprKind::Field { value, access: FieldAccess::Name(_) } if matches!(value.ty, Type::MutMap(_))) =>
            {
                let CoreExprKind::Field {
                    value: receiver,
                    access: FieldAccess::Name(method),
                } = &callee.kind
                else {
                    unreachable!()
                };
                let Type::MutMap(map_id) = receiver.ty else {
                    unreachable!()
                };
                let info = types.map_info(map_id);
                let Some(map) = (if method == "into_iter" {
                    self.lower_value(receiver, function_names, types)?
                } else {
                    self.lower_borrowed_value(receiver, function_names, types)?
                }) else {
                    return Ok(None);
                };
                let intrinsic = match method.as_str() {
                    "insert" => RuntimeIntrinsic::MutMapInsert(info.key, info.value),
                    "get" => RuntimeIntrinsic::MutMapGet(info.key, info.value),
                    "remove" => RuntimeIntrinsic::MutMapRemove(info.key, info.value),
                    "contains_key" => RuntimeIntrinsic::MutMapContainsKey(info.key),
                    "length" => RuntimeIntrinsic::MutMapLength,
                    "capacity" => RuntimeIntrinsic::MutMapCapacity,
                    "is_empty" => RuntimeIntrinsic::MutMapIsEmpty,
                    "into_iter" => RuntimeIntrinsic::MutMapIntoIter(info.key, info.value),
                    "to_list" => RuntimeIntrinsic::MutMapToList(info.key, info.value),
                    _ => return Err(Diagnostic::codegen("MIR MutMap method was not resolved")),
                };
                let destination = self.next_value(expression.ty);
                let mut values = vec![MirCallArgument {
                    parameter: 0,
                    value: map,
                }];
                for (index, argument) in arguments.iter().enumerate() {
                    let Some(value) = self.lower_value(&argument.value, function_names, types)?
                    else {
                        return Ok(None);
                    };
                    values.push(MirCallArgument {
                        parameter: index + 1,
                        value,
                    });
                }
                self.push_statement(MirStatement::RuntimeCall {
                    destination,
                    intrinsic,
                    arguments: values,
                });
                Ok(Some(destination))
            }
            CoreExprKind::Call {
                callee, arguments, ..
            } if matches!(&callee.kind, CoreExprKind::Field { value, access: FieldAccess::Name(_) } if matches!(value.ty, Type::MutSet(_))) =>
            {
                let CoreExprKind::Field {
                    value: receiver,
                    access: FieldAccess::Name(method),
                } = &callee.kind
                else {
                    unreachable!()
                };
                let Type::MutSet(set_id) = receiver.ty else {
                    unreachable!()
                };
                let element = types.map_info(set_id).key;
                let Some(set) = (if method == "into_iter" {
                    self.lower_value(receiver, function_names, types)?
                } else {
                    self.lower_borrowed_value(receiver, function_names, types)?
                }) else {
                    return Ok(None);
                };
                let intrinsic = match method.as_str() {
                    "add" => RuntimeIntrinsic::MutSetAdd(element),
                    "remove" => RuntimeIntrinsic::MutSetRemove(element),
                    "contains" => RuntimeIntrinsic::MutSetContains(element),
                    "length" => RuntimeIntrinsic::MutSetLength,
                    "capacity" => RuntimeIntrinsic::MutSetCapacity,
                    "is_empty" => RuntimeIntrinsic::MutSetIsEmpty,
                    "into_iter" => RuntimeIntrinsic::MutSetIntoIter(element),
                    "to_list" => RuntimeIntrinsic::MutSetToList(element),
                    _ => return Err(Diagnostic::codegen("MIR MutSet method was not resolved")),
                };
                let destination = self.next_value(expression.ty);
                let mut values = vec![MirCallArgument {
                    parameter: 0,
                    value: set,
                }];
                for (index, argument) in arguments.iter().enumerate() {
                    let Some(value) = self.lower_value(&argument.value, function_names, types)?
                    else {
                        return Ok(None);
                    };
                    values.push(MirCallArgument {
                        parameter: index + 1,
                        value,
                    });
                }
                self.push_statement(MirStatement::RuntimeCall {
                    destination,
                    intrinsic,
                    arguments: values,
                });
                Ok(Some(destination))
            }
            _ => unreachable!("collection call guard requires a builtin collection call"),
        }
    }
}
