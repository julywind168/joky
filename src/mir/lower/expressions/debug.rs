use super::super::*;
use crate::hir::{CoreCallArgument, CoreExpr, CoreExprKind};

impl Lowerer<'_> {
    pub(super) fn lower_show(
        &mut self,
        receiver: &CoreExpr,
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        if matches!(receiver.ty, Type::Dyn(_)) {
            let callee = CoreExpr {
                id: receiver.id,
                ty: Type::Unit,
                kind: CoreExprKind::Field {
                    value: Box::new(receiver.clone()),
                    access: crate::syntax::FieldAccess::Name(crate::sema::SHOW_METHOD.into()),
                },
            };
            let call = CoreExpr {
                id: receiver.id,
                ty: Type::String,
                kind: CoreExprKind::Call {
                    callee: Box::new(callee),
                    type_arguments: Vec::new(),
                    arguments: Vec::new(),
                    effect_operation: None,
                },
            };
            return self.lower_call(&call, function_names, types);
        }
        if let Some(symbol) = types
            .interface
            .method_symbols
            .get(&(receiver.ty, crate::sema::SHOW_METHOD.into()))
        {
            return self.lower_external_call(
                &CoreExpr {
                    id: receiver.id,
                    ty: Type::String,
                    kind: CoreExprKind::Unit,
                },
                symbol,
                &[CoreCallArgument {
                    label: None,
                    value: receiver.clone(),
                }],
                function_names,
                types,
            );
        }
        if types.has_show_impl(receiver.ty) {
            let Some(value) = self.lower_borrowed_value(receiver, function_names, types)? else {
                return Ok(None);
            };
            let method = *self
                .function_ids
                .get(&super::super::method_key(
                    receiver.ty,
                    crate::sema::SHOW_METHOD,
                ))
                .ok_or_else(|| Diagnostic::codegen("MIR Show method was not resolved"))?;
            let destination = self.next_value(Type::String);
            self.push_statement(MirStatement::MethodCall {
                destination,
                receiver: value,
                receiver_type: receiver.ty,
                method,
                arguments: Vec::new(),
                continuation: None,
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

    pub(super) fn lower_debug(
        &mut self,
        receiver: &CoreExpr,
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        if matches!(receiver.ty, Type::List(_)) {
            return self.lower_debug_list(receiver, function_names, types);
        }
        if types.has_default_debug(receiver.ty) {
            return self.lower_default_debug_call(receiver, function_names, types);
        }
        if receiver.ty.is_native_resource() {
            let Some(value) = self.lower_borrowed_value(receiver, function_names, types)? else {
                return Ok(None);
            };
            let id = self.next_value(Type::U64);
            self.push_statement(MirStatement::RuntimeCall {
                destination: id,
                intrinsic: RuntimeIntrinsic::DebugNativeId(receiver.ty),
                arguments: vec![MirCallArgument {
                    parameter: 0,
                    value,
                }],
            });
            let text = self.next_value(Type::String);
            self.push_statement(MirStatement::RuntimeCall {
                destination: text,
                intrinsic: RuntimeIntrinsic::Show(Type::U64),
                arguments: vec![MirCallArgument {
                    parameter: 0,
                    value: id,
                }],
            });
            let opening = self.debug_text(&format!("{}(id: ", crate::sema::type_name(receiver.ty)));
            let text = self.debug_concat(opening, text);
            let closing = self.debug_text(")");
            return Ok(Some(self.debug_concat(text, closing)));
        }
        if matches!(
            receiver.ty,
            Type::Tuple(_) | Type::Option(_) | Type::Result(_)
        ) {
            let Some(value) = self.lower_borrowed_value(receiver, function_names, types)? else {
                return Ok(None);
            };
            let result = self.lower_debug_aggregate(value, receiver.id, function_names, types)?;
            if result.is_some() {
                self.discard_value(Some(value));
            }
            return Ok(result);
        }
        let expression = CoreExpr {
            id: receiver.id,
            ty: Type::String,
            kind: CoreExprKind::Unit,
        };
        if let Some(symbol) = types
            .interface
            .method_symbols
            .get(&(receiver.ty, crate::sema::DEBUG_METHOD.into()))
        {
            return self.lower_external_call(
                &expression,
                symbol,
                &[CoreCallArgument {
                    label: None,
                    value: receiver.clone(),
                }],
                function_names,
                types,
            );
        }
        if matches!(receiver.ty, Type::Struct(_) | Type::Class(_) | Type::Dyn(_)) {
            let callee = CoreExpr {
                id: receiver.id,
                ty: Type::Unit,
                kind: CoreExprKind::Field {
                    value: Box::new(receiver.clone()),
                    access: crate::syntax::FieldAccess::Name(crate::sema::DEBUG_METHOD.into()),
                },
            };
            let call = CoreExpr {
                kind: CoreExprKind::Call {
                    callee: Box::new(callee),
                    type_arguments: vec![],
                    arguments: vec![],
                    effect_operation: None,
                },
                ..expression
            };
            return self.lower_call(&call, function_names, types);
        }
        let Some(value) = self.lower_value(receiver, function_names, types)? else {
            return Ok(None);
        };
        let destination = self.next_value(Type::String);
        if receiver.ty == Type::Unit {
            self.push_statement(MirStatement::Const {
                destination,
                value: MirConstant::String("()".into()),
            });
        } else {
            self.push_statement(MirStatement::RuntimeCall {
                destination,
                intrinsic: match receiver.ty {
                    Type::String => RuntimeIntrinsic::DebugString,
                    Type::Bytes => RuntimeIntrinsic::DebugBytes,
                    Type::Duration => RuntimeIntrinsic::DebugDuration,
                    _ => RuntimeIntrinsic::Show(receiver.ty),
                },
                arguments: vec![MirCallArgument {
                    parameter: 0,
                    value,
                }],
            });
        }
        Ok(Some(destination))
    }

    pub(super) fn lower_echo(
        &mut self,
        arguments: &[CoreCallArgument],
        discarded: bool,
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let argument = &arguments[0].value;
        let mut receiver = argument.clone();
        // Keep the result in one local while Debug borrows it. Only a used echo
        // result transfers ownership; a discarded echo observes its operand.
        if !discarded {
            let Some(value) = self.lower_value(argument, function_names, types)? else {
                return Ok(None);
            };
            let name = format!("@echo/{}", self.locals.len());
            let local = self.new_local(&name, argument.ty);
            self.bind_local(&name, local);
            let destination = self.next_value(Type::Unit);
            self.push_statement(MirStatement::Bind {
                local,
                value: Some(value),
                destination,
            });
            receiver.kind = CoreExprKind::Name(name);
        }
        let Some(displayed) = self.lower_debug(&receiver, function_names, types)? else {
            return Ok(None);
        };
        let Some(location) = self.lower_value(&arguments[1].value, function_names, types)? else {
            return Ok(None);
        };
        let destination = self.next_value(Type::Unit);
        self.push_statement(MirStatement::RuntimeCall {
            destination,
            intrinsic: RuntimeIntrinsic::Echo,
            arguments: vec![
                MirCallArgument {
                    parameter: 0,
                    value: location,
                },
                MirCallArgument {
                    parameter: 1,
                    value: displayed,
                },
            ],
        });
        if discarded {
            Ok(Some(destination))
        } else {
            self.lower_value(&receiver, function_names, types)
        }
    }
}
