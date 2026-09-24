use crate::diagnostic::SemanticError;
use crate::sema::checker::{
    intern_function, intern_list, intern_map, intern_option, intern_result, intern_tuple, Checker,
};
use crate::sema::expectation::TypeExpectation;
use crate::sema::types::Type;
use crate::sema::{DEBUG_METHOD, DROP_METHOD, SHOW_METHOD};
use crate::syntax::{CallArgument, Expr, ExprKind, FieldAccess, TypeAnnotation};
use crate::Span;

impl Checker {
    pub(super) fn check_method_call(
        &mut self,
        callee: &Expr,
        arguments: &[CallArgument],
        call_span: Span,
        call_id: crate::syntax::NodeId,
        expectation: TypeExpectation,
    ) -> Result<Type, SemanticError> {
        let ExprKind::Field {
            value: receiver,
            access: FieldAccess::Name(method_name),
        } = &callee.kind
        else {
            return Err(SemanticError::NotCallable { span: callee.span });
        };

        if let Ok(Type::Function(id)) = self.check_expression(callee, TypeExpectation::none()) {
            let signature = self
                .function_types
                .get(id)
                .cloned()
                .ok_or(SemanticError::NotCallable { span: callee.span })?;
            if arguments.len() != signature.parameters.len() {
                return Err(SemanticError::WrongArgumentCount {
                    function: method_name.clone(),
                    expected: signature.parameters.len(),
                    span: call_span,
                });
            }
            let parameters = signature
                .parameter_names
                .iter()
                .cloned()
                .zip(signature.parameters.iter().copied())
                .collect::<Vec<_>>();
            for (argument, (_, expected_type)) in self
                .order_arguments(arguments, &parameters, method_name, call_span)?
                .into_iter()
                .zip(&parameters)
            {
                self.check_expression(argument, TypeExpectation::require(*expected_type))?;
            }
            self.current_effects.extend(&signature.effects);
            return Ok(signature.return_type);
        }

        let receiver_type = self.check_expression(receiver, TypeExpectation::none())?;
        if receiver_type.is_numeric()
            && matches!(method_name.as_str(), "abs" | "min" | "max" | "to")
        {
            return self.check_numeric_method(
                receiver,
                receiver_type,
                method_name,
                arguments,
                call_span,
                call_id,
            );
        }
        let owner = match receiver_type {
            Type::Option(_) => Some("Option"),
            Type::Result(_) => Some("Result"),
            _ => None,
        };
        if let Some(owner) = owner {
            let name = format!("@prelude/{owner}/{method_name}");
            if crate::sema::prelude::is_method(&name) && self.functions.contains_key(&name) {
                let callee = Expr {
                    kind: ExprKind::Name(name),
                    ..callee.clone()
                };
                let mut forwarded = Vec::with_capacity(arguments.len() + 1);
                forwarded.push(CallArgument {
                    label: None,
                    value: (**receiver).clone(),
                });
                forwarded.extend_from_slice(arguments);
                return self.check_call(&callee, &forwarded, call_span, call_id, expectation);
            }
        }
        if let Type::Param(id) = receiver_type {
            let trait_name = self.bound_method_candidate(id, method_name, callee.span)?;
            if trait_name == "Drop" {
                return Err(SemanticError::FunctionNotSupported {
                    name: "Drop.drop is invoked automatically and cannot be called directly".into(),
                    span: call_span,
                });
            }
            let name =
                crate::sema::trait_method_name(self.definition_module, &trait_name, method_name);
            self.resolved_method_calls.insert(
                call_id,
                crate::sema::methods::ResolvedMethodCall {
                    name,
                    qualified: false,
                    static_target: None,
                },
            );
            return self.check_bound_method(
                receiver_type,
                &trait_name,
                method_name,
                arguments,
                call_span,
            );
        }
        let qualified = self
            .resolved_method_calls
            .get(&call_id)
            .is_some_and(|call| call.qualified);
        let selected_name = if qualified {
            method_name.clone()
        } else if let Some(name) =
            self.nominal_method_candidate(receiver_type, method_name, callee.span)?
        {
            name
        } else if let Type::Dyn(id) = receiver_type {
            self.dynamic_types[id]
                .methods
                .iter()
                .find(|method| method.name == *method_name)
                .map(|method| method.implementation_name.clone())
                .unwrap_or_else(|| method_name.clone())
        } else if method_name == "advance"
            && matches!(
                receiver_type,
                Type::List(_)
                    | Type::BytesCursor
                    | Type::MapCursor(_)
                    | Type::MapKeyCursor(_)
                    | Type::MapValueCursor(_)
                    | Type::MutListCursor(_)
                    | Type::MutMapCursor(_)
                    | Type::MutSetCursor(_)
            )
        {
            crate::sema::CURSOR_METHOD.into()
        } else if method_name == "equals" && self.implements_trait(receiver_type, "PartialEq") {
            crate::sema::PARTIAL_EQ_METHOD.into()
        } else if method_name == "hash" && self.implements_trait(receiver_type, "Hash") {
            crate::sema::HASH_METHOD.into()
        } else if method_name == "partial_compare"
            && self.implements_trait(receiver_type, "PartialOrd")
        {
            crate::sema::PARTIAL_ORD_METHOD.into()
        } else if method_name == "compare" && self.implements_trait(receiver_type, "Ord") {
            crate::sema::ORD_METHOD.into()
        } else if method_name == "show" {
            SHOW_METHOD.into()
        } else if method_name == "debug" {
            DEBUG_METHOD.into()
        } else {
            method_name.clone()
        };
        if selected_name != *method_name {
            self.resolved_method_calls.insert(
                call_id,
                crate::sema::methods::ResolvedMethodCall {
                    name: selected_name.clone(),
                    qualified,
                    static_target: None,
                },
            );
        }
        let method_name = &selected_name;

        if method_name == crate::sema::CURSOR_METHOD
            && matches!(
                receiver_type,
                Type::List(_)
                    | Type::BytesCursor
                    | Type::MapCursor(_)
                    | Type::MapKeyCursor(_)
                    | Type::MapValueCursor(_)
                    | Type::MutListCursor(_)
                    | Type::MutMapCursor(_)
                    | Type::MutSetCursor(_)
            )
        {
            return self.check_bound_method(
                receiver_type,
                "Cursor",
                "advance",
                arguments,
                call_span,
            );
        }

        if method_name == crate::sema::HASH_METHOD {
            if !self.implements_trait(receiver_type, "Hash") {
                return Err(SemanticError::TraitBoundNotSatisfied {
                    trait_name: "Hash".into(),
                    actual: crate::sema::type_name(receiver_type),
                    span: call_span,
                });
            }
            return self.check_bound_method(receiver_type, "Hash", "hash", arguments, call_span);
        }
        if receiver_type == Type::Hasher && method_name == "finish" {
            if !arguments.is_empty() {
                return Err(SemanticError::WrongArgumentCount {
                    function: "Hasher.finish".into(),
                    expected: 0,
                    span: call_span,
                });
            }
            return Ok(Type::U64);
        }

        if matches!(
            method_name.as_str(),
            crate::sema::PARTIAL_ORD_METHOD | crate::sema::ORD_METHOD
        ) {
            let (trait_name, method) = if method_name == crate::sema::ORD_METHOD {
                ("Ord", "compare")
            } else {
                ("PartialOrd", "partial_compare")
            };
            if !self.implements_trait(receiver_type, trait_name) {
                return Err(SemanticError::TraitBoundNotSatisfied {
                    trait_name: trait_name.into(),
                    actual: crate::sema::type_name(receiver_type),
                    span: call_span,
                });
            }
            return self.check_bound_method(
                receiver_type,
                trait_name,
                method,
                arguments,
                call_span,
            );
        }
        if method_name == crate::sema::PARTIAL_EQ_METHOD {
            if !self.implements_trait(receiver_type, "PartialEq") {
                return Err(SemanticError::TraitBoundNotSatisfied {
                    trait_name: "PartialEq".into(),
                    actual: crate::sema::type_name(receiver_type),
                    span: call_span,
                });
            }
            return self.check_bound_method(
                receiver_type,
                "PartialEq",
                "equals",
                arguments,
                call_span,
            );
        }
        if receiver_type.is_native_resource() && method_name == DEBUG_METHOD {
            if !arguments.is_empty() {
                return Err(SemanticError::WrongArgumentCount {
                    function: "debug".into(),
                    expected: 0,
                    span: call_span,
                });
            }
            return Ok(Type::String);
        }
        if let Type::Dyn(id) = receiver_type {
            let method = self.dynamic_types[id]
                .methods
                .iter()
                .find(|method| method.implementation_name == *method_name)
                .cloned()
                .ok_or_else(|| SemanticError::UnknownFunction {
                    name: method_name.clone(),
                    span: call_span,
                })?;
            let signature = self.function_types[method.signature].clone();
            let parameters = signature
                .parameter_names
                .iter()
                .cloned()
                .zip(signature.parameters.iter().copied())
                .collect::<Vec<_>>();
            if arguments.len() != parameters.len() {
                return Err(SemanticError::WrongArgumentCount {
                    function: method_name.clone(),
                    expected: parameters.len(),
                    span: call_span,
                });
            }
            for (argument, (_, ty)) in self
                .order_arguments(arguments, &parameters, method_name, call_span)?
                .into_iter()
                .zip(&parameters)
            {
                self.check_expression(argument, TypeExpectation::require(*ty))?;
            }
            self.current_effects.extend(&signature.effects);
            return Ok(signature.return_type);
        }
        if receiver_type == Type::CCallback {
            if !arguments.is_empty() {
                return Err(SemanticError::WrongArgumentCount {
                    function: format!("CCallback.{method_name}"),
                    expected: 0,
                    span: call_span,
                });
            }
            return match method_name.as_str() {
                "function" | "context" => self.apply_type_value(
                    if method_name == "function" {
                        "CPtr"
                    } else {
                        "CMutPtr"
                    },
                    &[(None, Type::Unit, call_span)],
                    call_span,
                    &mut Vec::new(),
                ),
                "close" => Ok(Type::Unit),
                "failed" => Ok(Type::Bool),
                _ => Err(SemanticError::UnknownFunction {
                    name: format!("CCallback.{method_name}"),
                    span: call_span,
                }),
            };
        }
        if method_name == DROP_METHOD {
            return Err(SemanticError::FunctionNotSupported {
                name: "Drop.drop is invoked automatically and cannot be called directly".into(),
                span: call_span,
            });
        }
        if receiver_type.is_c_pointer() && method_name == "is_null" {
            if !arguments.is_empty() {
                return Err(SemanticError::WrongArgumentCount {
                    function: "is_null".to_owned(),
                    expected: 0,
                    span: call_span,
                });
            }
            return Ok(Type::Bool);
        }
        if receiver_type == Type::CStr && method_name == "to_string" {
            if !arguments.is_empty() {
                return Err(SemanticError::WrongArgumentCount {
                    function: "CStr.to_string".to_owned(),
                    expected: 0,
                    span: call_span,
                });
            }
            return Ok(Type::Option(intern_option(
                &mut self.option_types,
                Type::String,
            )));
        }
        if let Type::CMutPtr(id) = receiver_type {
            let content = self.c_pointers[id];
            match method_name.as_str() {
                "read" => {
                    if !arguments.is_empty() {
                        return Err(SemanticError::WrongArgumentCount {
                            function: "CMutPtr.read".to_owned(),
                            expected: 0,
                            span: call_span,
                        });
                    }
                    return Ok(content);
                }
                "write" => {
                    if arguments.len() != 1 {
                        return Err(SemanticError::WrongArgumentCount {
                            function: "CMutPtr.write".to_owned(),
                            expected: 1,
                            span: call_span,
                        });
                    }
                    self.check_expression(&arguments[0].value, TypeExpectation::require(content))?;
                    return Ok(Type::Unit);
                }
                "free" => {
                    if !arguments.is_empty() {
                        return Err(SemanticError::WrongArgumentCount {
                            function: "CMutPtr.free".to_owned(),
                            expected: 0,
                            span: call_span,
                        });
                    }
                    return Ok(Type::Unit);
                }
                _ => {}
            }
        }
        let intrinsic_name = match receiver_type {
            Type::String => Some("String"),
            Type::Bytes => Some("Bytes"),
            Type::Map(_) => Some("Map"),
            Type::List(_) => Some("List"),
            Type::MutBytes => Some("MutBytes"),
            Type::MutList(_) => Some("MutList"),
            Type::MutMap(_) => Some("MutMap"),
            Type::MutSet(_) => Some("MutSet"),
            _ => None,
        };
        if let Some(intrinsic_name) = intrinsic_name.filter(|_| !method_name.starts_with("@trait:"))
        {
            if let Some(method) = self
                .intrinsic_methods
                .get(intrinsic_name)
                .and_then(|methods| methods.get(method_name))
                .cloned()
            {
                let type_count = method.type_parameters.len();
                if arguments.len() != type_count + method.parameters.len() {
                    return Err(SemanticError::WrongArgumentCount {
                        function: method_name.clone(),
                        expected: type_count + method.parameters.len(),
                        span: call_span,
                    });
                }
                let type_arguments = if type_count == 0 {
                    Vec::new()
                } else {
                    self.resolve_type_value_arguments(
                        call_id,
                        &arguments[..type_count],
                        type_count,
                    )?
                    .ok_or_else(|| SemanticError::FunctionNotSupported {
                        name: format!("{method_name} requires compile-time type arguments"),
                        span: call_span,
                    })?
                };
                self.check_intrinsic_constraints(
                    receiver_type,
                    &method,
                    &type_arguments,
                    call_span,
                )?;
                let consuming_into_iter = method_name == "into_iter"
                    && method.receiver_mode == crate::syntax::ReceiverMode::Owned
                    && method.parameters.is_empty();
                if (!consuming_into_iter
                    && method.receiver_mode != crate::syntax::ReceiverMode::Borrowed)
                    || method.parameters.iter().any(|parameter| parameter.borrowed)
                {
                    return Err(SemanticError::FunctionNotSupported {
                        name: "value and collection intrinsics require &self and owned arguments"
                            .to_owned(),
                        span: method.span,
                    });
                }
                let runtime_arguments = &arguments[type_arguments.len()..];
                for (argument, parameter) in runtime_arguments.iter().zip(&method.parameters) {
                    let expected = self.resolve_intrinsic_annotation(
                        receiver_type,
                        &parameter.ty,
                        &method.type_parameters,
                        &type_arguments,
                    )?;
                    self.check_expression(&argument.value, TypeExpectation::require(expected))?;
                }
                let result = self.resolve_intrinsic_annotation(
                    receiver_type,
                    &method.return_type,
                    &method.type_parameters,
                    &type_arguments,
                )?;
                if method_name == "parse" {
                    let target = type_arguments[0];
                    self.resolved_method_calls.insert(
                        call_id,
                        crate::sema::methods::ResolvedMethodCall {
                            name: crate::sema::FROM_STRING_METHOD.into(),
                            qualified: false,
                            static_target: Some(target),
                        },
                    );
                }
                return Ok(result);
            } else if self.intrinsic_methods.contains_key(intrinsic_name) {
                return Err(SemanticError::UnknownFunction {
                    name: method_name.clone(),
                    span: callee.span,
                });
            }
        }
        if receiver_type.is_native_resource() {
            let operation_id = self
                .intrinsic_effect_methods
                .get(&(receiver_type, method_name.clone()))
                .copied()
                .ok_or_else(|| SemanticError::UnknownFunction {
                    name: format!("{}.{method_name}", crate::sema::type_name(receiver_type)),
                    span: callee.span,
                })?;
            let operation = self
                .effects
                .operation_info(operation_id)
                .expect("checked intrinsic effect binding")
                .clone();
            if arguments.len() != operation.parameters.len().saturating_sub(1) {
                return Err(SemanticError::WrongArgumentCount {
                    function: format!("{receiver_type:?}.{method_name}"),
                    expected: operation.parameters.len().saturating_sub(1),
                    span: call_span,
                });
            }
            for (argument, expected) in arguments.iter().zip(operation.parameters.iter().skip(1)) {
                self.check_expression(&argument.value, TypeExpectation::require(*expected))?;
            }
            self.current_effects.insert(operation_id);
            self.effect_operations.insert(call_id, operation_id);
            return Ok(operation.return_type);
        }
        if method_name == SHOW_METHOD || method_name == DEBUG_METHOD {
            let debug = method_name == DEBUG_METHOD;
            if !arguments.is_empty() {
                return Err(SemanticError::WrongArgumentCount {
                    function: method_name.clone(),
                    expected: 0,
                    span: call_span,
                });
            }
            if !(if debug {
                self.implements_debug(receiver_type)
            } else {
                self.implements_show(receiver_type)
            }) {
                return Err(SemanticError::TraitBoundNotSatisfied {
                    trait_name: if debug { "Debug" } else { "Show" }.to_owned(),
                    actual: crate::sema::type_name(receiver_type),
                    span: receiver.span,
                });
            }
            return Ok(Type::String);
        }
        let signature = match receiver_type {
            Type::Class(class_id) => self
                .classes
                .values()
                .find(|class| class.id == class_id)
                .and_then(|class| class.methods.get(method_name)),
            Type::Struct(struct_id) => self
                .structs
                .values()
                .find(|structure| structure.id == struct_id)
                .and_then(|structure| structure.methods.get(method_name)),
            _ => None,
        }
        .cloned()
        .ok_or_else(|| SemanticError::UnknownFunction {
            name: method_name.clone(),
            span: callee.span,
        })?;
        if arguments.len() != signature.parameters.len() {
            return Err(SemanticError::WrongArgumentCount {
                function: method_name.clone(),
                expected: signature.parameters.len(),
                span: call_span,
            });
        }
        for (argument, (_, expected_type)) in self
            .order_arguments(arguments, &signature.parameters, method_name, call_span)?
            .into_iter()
            .zip(&signature.parameters)
        {
            self.check_expression(argument, TypeExpectation::require(*expected_type))?;
        }
        self.current_effects.extend(&signature.used_effects);
        Ok(signature.return_type)
    }

    fn resolve_intrinsic_annotation(
        &mut self,
        receiver: Type,
        annotation: &TypeAnnotation,
        type_parameters: &[crate::syntax::TypeParameter],
        type_arguments: &[Type],
    ) -> Result<Type, SemanticError> {
        if let Some(name) = annotation.as_name() {
            if let Some(index) = type_parameters.iter().position(|p| p.name == name) {
                return type_arguments.get(index).copied().ok_or_else(|| {
                    SemanticError::UnknownType {
                        name: name.to_owned(),
                        span: annotation.span,
                    }
                });
            }
            return self.resolve_intrinsic_type(receiver, name, annotation.span);
        }
        if let crate::syntax::TypeExpr::Tuple(elements) = &annotation.kind {
            let values = elements
                .iter()
                .map(|element| {
                    self.resolve_intrinsic_annotation(
                        receiver,
                        element,
                        type_parameters,
                        type_arguments,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?;
            return Ok(if values.is_empty() {
                Type::Unit
            } else {
                Type::Tuple(intern_tuple(&mut self.tuple_types, values))
            });
        }
        if let crate::syntax::TypeExpr::Function { parameters, result } = &annotation.kind {
            let parameters = parameters
                .iter()
                .map(|parameter| {
                    Ok((
                        parameter.label.clone().unwrap_or_default(),
                        self.resolve_intrinsic_annotation(
                            receiver,
                            &parameter.value,
                            type_parameters,
                            type_arguments,
                        )?,
                    ))
                })
                .collect::<Result<Vec<_>, SemanticError>>()?;
            let (parameter_names, parameters): (Vec<_>, Vec<_>) = parameters.into_iter().unzip();
            let return_type = self.resolve_intrinsic_annotation(
                receiver,
                result,
                type_parameters,
                type_arguments,
            )?;
            return Ok(Type::Function(intern_function(
                &mut self.function_types,
                parameter_names,
                parameters,
                return_type,
            )));
        }
        let crate::syntax::TypeExpr::Apply { callee, arguments } = &annotation.kind else {
            return Err(SemanticError::UnknownType {
                name: annotation.to_string(),
                span: annotation.span,
            });
        };
        let Some(name) = callee.as_name() else {
            return Err(SemanticError::UnknownType {
                name: callee.to_string(),
                span: annotation.span,
            });
        };
        let values = arguments
            .iter()
            .map(|argument| {
                self.resolve_intrinsic_annotation(
                    receiver,
                    &argument.value,
                    type_parameters,
                    type_arguments,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        match (name, values.as_slice()) {
            ("List", [value]) => Ok(Type::List(intern_list(&mut self.list_types, *value))),
            ("Map", [key, value]) => Ok(Type::Map(intern_map(&mut self.maps, *key, *value))),
            ("MapCursor", [key, value]) => {
                // Pre-intern the entry tuple so `Cursor::Item` resolves later.
                let _ = intern_tuple(&mut self.tuple_types, vec![*key, *value]);
                Ok(Type::MapCursor(intern_map(&mut self.maps, *key, *value)))
            }
            ("MapKeyCursor", [key, value]) => {
                let _ = intern_tuple(&mut self.tuple_types, vec![*key, *value]);
                Ok(Type::MapKeyCursor(intern_map(&mut self.maps, *key, *value)))
            }
            ("MapValueCursor", [key, value]) => {
                let _ = intern_tuple(&mut self.tuple_types, vec![*key, *value]);
                Ok(Type::MapValueCursor(intern_map(
                    &mut self.maps,
                    *key,
                    *value,
                )))
            }
            ("MutListCursor", [value]) => {
                let cursor = Type::MutListCursor(intern_list(&mut self.list_types, *value));
                intern_cursor_pair(
                    &mut self.tuple_types,
                    &mut self.option_types,
                    *value,
                    cursor,
                );
                Ok(cursor)
            }
            ("MutMapCursor", [key, value]) => {
                let _ = intern_tuple(&mut self.tuple_types, vec![*key, *value]);
                let cursor = Type::MutMapCursor(intern_map(&mut self.maps, *key, *value));
                let item = Type::Tuple(intern_tuple(&mut self.tuple_types, vec![*key, *value]));
                intern_cursor_pair(&mut self.tuple_types, &mut self.option_types, item, cursor);
                Ok(cursor)
            }
            ("MutSetCursor", [value]) => {
                let cursor = Type::MutSetCursor(intern_map(&mut self.maps, *value, Type::Bool));
                intern_cursor_pair(
                    &mut self.tuple_types,
                    &mut self.option_types,
                    *value,
                    cursor,
                );
                Ok(cursor)
            }
            ("Option", [value]) => Ok(Type::Option(intern_option(&mut self.option_types, *value))),
            ("Result", [ok, err]) => Ok(Type::Result(intern_result(
                &mut self.result_types,
                *ok,
                *err,
            ))),
            _ => Err(SemanticError::UnknownType {
                name: annotation.to_string(),
                span: annotation.span,
            }),
        }
    }

    pub(in crate::sema) fn resolve_intrinsic_type(
        &self,
        receiver: Type,
        name: &str,
        span: Span,
    ) -> Result<Type, SemanticError> {
        let type_for_name = |name: &str| {
            let (owner, arguments) = match receiver {
                Type::List(id) => ("List", vec![self.list_types[id]]),
                Type::Map(id) => ("Map", vec![self.maps[id].key, self.maps[id].value]),
                Type::MutList(id) => ("MutList", vec![self.list_types[id]]),
                Type::MutMap(id) => ("MutMap", vec![self.maps[id].key, self.maps[id].value]),
                Type::MutSet(id) => ("MutSet", vec![self.maps[id].key]),
                _ => return None,
            };
            self.intrinsic_parameter_names
                .get(owner)?
                .iter()
                .position(|p| p == name)
                .and_then(|index| arguments.get(index).copied())
        };
        type_for_name(name)
            .or_else(|| crate::sema::types::primitive_type(name))
            .ok_or_else(|| SemanticError::UnknownType {
                name: name.to_owned(),
                span,
            })
    }

    fn check_numeric_method(
        &mut self,
        receiver: &Expr,
        receiver_type: Type,
        method_name: &str,
        arguments: &[CallArgument],
        call_span: Span,
        call_id: crate::syntax::NodeId,
    ) -> Result<Type, SemanticError> {
        match method_name {
            "abs" => {
                if !arguments.is_empty() {
                    return Err(SemanticError::WrongArgumentCount {
                        function: method_name.to_owned(),
                        expected: 0,
                        span: call_span,
                    });
                }
                if receiver_type.is_signed_integer() {
                    let minimum = match receiver_type {
                        Type::I8 => i8::MIN as i128,
                        Type::I16 => i16::MIN as i128,
                        Type::I32 => i32::MIN as i128,
                        Type::I64 => i64::MIN as i128,
                        _ => 0,
                    };
                    if crate::sema::validation::constant_integer(receiver) == Some(minimum) {
                        return Err(SemanticError::AbsoluteValueOverflow { span: call_span });
                    }
                }
                Ok(receiver_type)
            }
            "min" | "max" => {
                if arguments.len() != 1 {
                    return Err(SemanticError::WrongArgumentCount {
                        function: method_name.to_owned(),
                        expected: 1,
                        span: call_span,
                    });
                }
                self.check_expression(
                    &arguments[0].value,
                    TypeExpectation::require(receiver_type),
                )?;
                Ok(receiver_type)
            }
            "to" => {
                let Some(target) = self
                    .resolve_type_value_arguments(call_id, arguments, 1)?
                    .and_then(|types| types.into_iter().next())
                else {
                    return Err(SemanticError::NumericConversionTarget { span: call_span });
                };
                if !target.is_numeric() {
                    return Err(SemanticError::NumericConversionTarget { span: call_span });
                }
                Ok(target)
            }
            _ => Err(SemanticError::NotCallable { span: call_span }),
        }
    }
}

fn intern_cursor_pair(
    tuples: &mut Vec<Vec<Type>>,
    options: &mut Vec<Type>,
    item: Type,
    cursor: Type,
) {
    let pair = Type::Tuple(intern_tuple(tuples, vec![item, cursor]));
    intern_option(options, pair);
}
