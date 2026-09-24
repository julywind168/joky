use crate::diagnostic::SemanticError;
use crate::syntax::{CallArgument, Expr, ExprKind, FieldAccess};
use crate::Span;

use super::checker::{intern_list, intern_map, Checker};
use super::expectation::TypeExpectation;
use super::types::Type;

mod arguments;
mod builtins;
mod collections;
mod generics;
mod methods;
mod qualified;

fn effect_path(expression: &Expr) -> Option<String> {
    match &expression.kind {
        ExprKind::Name(name) => Some(name.clone()),
        ExprKind::Field {
            value,
            access: FieldAccess::Name(name),
        } => Some(format!("{}.{name}", effect_path(value)?)),
        _ => None,
    }
}

impl Checker {
    /// Batch-1 native callbacks must be zero-capture closures: C receives a
    /// static trampoline compiled for that exact closure, and a captured
    /// environment would need a lifetime protocol that does not exist yet.
    pub(super) fn require_zero_capture_callback(
        &self,
        argument: &Expr,
    ) -> Result<(), SemanticError> {
        let ExprKind::Closure { .. } = &argument.kind else {
            return Err(SemanticError::FunctionNotSupported {
                name: "native C callbacks must be written as a closure literal at the call site (capturing callbacks are not supported yet)".into(),
                span: argument.span,
            });
        };
        let captures = self
            .closure_captures
            .get(&argument.id)
            .map(Vec::len)
            .unwrap_or(0);
        if captures != 0 {
            return Err(SemanticError::FunctionNotSupported {
                name: "native C callbacks cannot capture values yet; use a capture-free closure"
                    .into(),
                span: argument.span,
            });
        }
        Ok(())
    }

    pub(super) fn check_call(
        &mut self,
        callee: &Expr,
        arguments: &[CallArgument],
        call_span: Span,
        call_id: crate::syntax::NodeId,
        expectation: TypeExpectation,
    ) -> Result<Type, SemanticError> {
        if matches!(&callee.kind, ExprKind::Name(name) if name == "Hasher") {
            if !arguments.is_empty() {
                return Err(SemanticError::WrongArgumentCount {
                    function: "Hasher".into(),
                    expected: 0,
                    span: call_span,
                });
            }
            return Ok(Type::Hasher);
        }
        if let Some(ty) = self.qualified_trait_call(callee, arguments, call_span, call_id)? {
            return Ok(ty);
        }
        if let Some(Type::Dyn(id)) = self.resolve_type_value_expr(callee)? {
            if arguments.len() != 1 || arguments[0].label.is_some() {
                return Err(SemanticError::WrongArgumentCount {
                    function: "Dyn constructor".into(),
                    expected: 1,
                    span: call_span,
                });
            }
            let concrete = self.check_expression(&arguments[0].value, TypeExpectation::none())?;
            let info = self.dynamic_types[id].clone();
            if let Type::Dyn(source) = concrete {
                self.dynamic_types[source]
                    .upcast_slots(&info, |id| &self.function_types[id])
                    .map_err(|message| super::dynamic::invalid(message, call_span))?;
                self.types.insert(callee.id, Type::Dyn(id));
                return Ok(Type::Dyn(id));
            }
            for trait_name in &info.trait_names {
                if (trait_name != "Debug" && !matches!(concrete, Type::Struct(_) | Type::Class(_)))
                    || !self.implements_trait(concrete, trait_name)
                {
                    return Err(SemanticError::TraitBoundNotSatisfied {
                        trait_name: trait_name.clone(),
                        actual: crate::sema::type_name(concrete),
                        span: call_span,
                    });
                }
            }
            for (name, expected) in &info.bindings {
                let actual = info.trait_names.iter().find_map(|trait_name| {
                    self.trait_associated_impls
                        .get(&(trait_name.clone(), concrete))
                        .and_then(|bindings| bindings.get(name))
                });
                if actual != Some(expected) {
                    return Err(super::dynamic::invalid(
                        format!("Dyn associated type '{name}' does not match the implementation"),
                        call_span,
                    ));
                }
            }
            if !self.drop_contract(concrete).1.is_empty() {
                return Err(super::dynamic::invalid(
                    "Dyn cannot erase a value with effectful Drop",
                    call_span,
                ));
            }
            for method in &info.methods {
                if method.implementation_name == crate::sema::DEBUG_METHOD
                    && info.includes("Debug")
                    && !self
                        .trait_impls
                        .get("Debug")
                        .is_some_and(|types| types.contains(&concrete))
                {
                    continue;
                }
                let implementation = match concrete {
                    Type::Struct(id) => self
                        .structs
                        .values()
                        .find(|info| info.id == id)
                        .and_then(|info| info.methods.get(&method.implementation_name)),
                    Type::Class(id) => self
                        .classes
                        .values()
                        .find(|info| info.id == id)
                        .and_then(|info| info.methods.get(&method.implementation_name)),
                    _ => None,
                }
                .expect("checked trait implementation");
                let allowed = &self.function_types[method.signature].effects;
                for group in implementation.declared_effects.iter() {
                    if self
                        .effects
                        .all_operations(group)
                        .into_iter()
                        .any(|op| !allowed.contains(op))
                    {
                        return Err(super::dynamic::invalid(format!("dynamic method '{}' must declare the implementation's effects in the trait", method.name), call_span));
                    }
                }
            }
            self.types.insert(callee.id, Type::Dyn(id));
            return Ok(Type::Dyn(id));
        }
        if matches!(&callee.kind, ExprKind::Call { .. }) {
            if let Some(type_value) = self.resolve_type_value_expr(callee)? {
                if matches!(
                    type_value,
                    Type::List(_)
                        | Type::MutList(_)
                        | Type::Map(_)
                        | Type::MutMap(_)
                        | Type::MutSet(_)
                ) {
                    if !arguments.is_empty() {
                        return Err(SemanticError::WrongArgumentCount {
                            function: crate::sema::type_name(type_value),
                            expected: 0,
                            span: call_span,
                        });
                    }
                    return Ok(type_value);
                }
            }
        }
        if let ExprKind::Call { .. } = &callee.kind {
            if let Some(Type::Struct(struct_id)) = self.resolve_type_value_expr(callee)? {
                let definition = self
                    .structs
                    .values()
                    .find(|info| info.id == struct_id)
                    .cloned()
                    .expect("generated struct has a definition");
                for argument in arguments {
                    let field_type = definition
                        .fields
                        .iter()
                        .find(|(name, _)| argument.label.as_deref() == Some(name))
                        .map(|(_, ty)| *ty)
                        .ok_or_else(|| SemanticError::UnknownStructField {
                            struct_name: format!("struct_{struct_id}"),
                            field: argument.label.clone().unwrap_or_default(),
                            span: argument.value.span,
                        })?;
                    self.check_expression(&argument.value, TypeExpectation::require(field_type))?;
                }
                let required = definition
                    .defaults
                    .iter()
                    .filter(|default| default.is_none())
                    .count();
                if arguments.len() < required || arguments.len() > definition.fields.len() {
                    return Err(SemanticError::WrongArgumentCount {
                        function: format!("struct_{struct_id}"),
                        expected: required,
                        span: call_span,
                    });
                }
                return Ok(Type::Struct(struct_id));
            }
        }
        if matches!(&callee.kind, ExprKind::Name(name)
            if self.lookup(name).is_none() && self.type_functions.contains_key(name))
        {
            return Err(SemanticError::FunctionNotSupported {
                name: "type function results cannot be used as runtime values".to_owned(),
                span: call_span,
            });
        }
        // A module-qualified nominal constructor is a type value, not an
        // exported function. Reuse the ordinary constructor's field checks.
        if matches!(callee.kind, ExprKind::Field { .. }) {
            let nominal = self.resolve_type_value_expr(callee)?;
            let name = match nominal {
                Some(Type::Struct(id)) => self
                    .structs
                    .iter()
                    .find(|(_, info)| info.id == id)
                    .map(|(name, _)| name.clone()),
                Some(Type::Class(id)) => self
                    .classes
                    .iter()
                    .find(|(_, info)| info.id == id)
                    .map(|(name, _)| name.clone()),
                _ => None,
            };
            if let Some(name) = name {
                self.types.insert(callee.id, nominal.unwrap());
                let named = Expr {
                    kind: ExprKind::Name(name),
                    ..callee.clone()
                };
                return self.check_call(&named, arguments, call_span, call_id, expectation);
            }
        }
        if let Some(ty) =
            self.check_import_call(callee, arguments, call_span, call_id, expectation)?
        {
            return Ok(ty);
        }
        if let Some(ty) =
            self.check_builtin_call(callee, arguments, call_span, call_id, expectation)?
        {
            return Ok(ty);
        }
        if let ExprKind::Field {
            value,
            access: FieldAccess::Name(operation_name),
        } = &callee.kind
        {
            if let Some(effect_name) = effect_path(value) {
                if let Some(effect_id) = self.effects.by_name(&effect_name) {
                    let operation_id = self
                        .effects
                        .operation_by_name(effect_id, operation_name)
                        .ok_or_else(|| SemanticError::UnknownEffectOperation {
                            effect: effect_name.clone(),
                            operation: operation_name.clone(),
                            span: callee.span,
                        })?;
                    let operation = self
                        .effects
                        .operation_info(operation_id)
                        .expect("registered effect operation")
                        .clone();
                    if arguments.len() != operation.parameters.len() {
                        return Err(SemanticError::WrongArgumentCount {
                            function: format!("{effect_name}.{operation_name}"),
                            expected: operation.parameters.len(),
                            span: call_span,
                        });
                    }
                    for (argument, expected) in arguments.iter().zip(&operation.parameters) {
                        self.check_expression(
                            &argument.value,
                            TypeExpectation::require(*expected),
                        )?;
                    }
                    self.current_effects.insert(operation_id);
                    self.effect_operations.insert(call_id, operation_id);
                    return Ok(operation.return_type);
                }
            }
        }

        if let ExprKind::Field {
            value,
            access: FieldAccess::Name(variant_name),
        } = &callee.kind
        {
            if matches!(&value.kind, ExprKind::Name(name) if name == "Bytes")
                && variant_name == "from_string"
            {
                if arguments.len() != 1 {
                    return Err(SemanticError::WrongArgumentCount {
                        function: "Bytes.from_string".to_owned(),
                        expected: 1,
                        span: call_span,
                    });
                }
                self.check_expression(&arguments[0].value, TypeExpectation::require(Type::String))?;
                return Ok(Type::Bytes);
            }
            if matches!(&value.kind, ExprKind::Name(name) if name == "MutBytes") {
                let expected = match variant_name.as_str() {
                    "with_capacity" | "from_bytes" => 1,
                    _ => {
                        return Err(SemanticError::UnknownFunction {
                            name: variant_name.clone(),
                            span: callee.span,
                        })
                    }
                };
                if arguments.len() != expected {
                    return Err(SemanticError::WrongArgumentCount {
                        function: format!("MutBytes.{variant_name}"),
                        expected,
                        span: call_span,
                    });
                }
                let argument_type = if variant_name == "with_capacity" {
                    Type::U64
                } else {
                    Type::Bytes
                };
                self.check_expression(
                    &arguments[0].value,
                    TypeExpectation::require(argument_type),
                )?;
                return Ok(Type::MutBytes);
            }
            if matches!(&value.kind, ExprKind::Name(name) if name == "List")
                && variant_name == "empty"
            {
                let Some(type_values) = self.resolve_type_value_arguments(call_id, arguments, 1)?
                else {
                    return Err(SemanticError::WrongArgumentCount {
                        function: "List.empty".to_owned(),
                        expected: 0,
                        span: call_span,
                    });
                };
                if type_values.len() != 1 {
                    return Err(SemanticError::WrongArgumentCount {
                        function: "List.empty".to_owned(),
                        expected: 0,
                        span: call_span,
                    });
                }
                let element = type_values[0];
                self.check_list_element(element, call_span)?;
                return Ok(Type::List(intern_list(&mut self.list_types, element)));
            }
            if matches!(&value.kind, ExprKind::Name(name) if name == "Map" || name == "Set")
                && variant_name == "empty"
            {
                let is_set = matches!(&value.kind, ExprKind::Name(name) if name == "Set");
                let expected_type_arguments = if is_set { 1 } else { 2 };
                let Some(type_values) =
                    self.resolve_type_value_arguments(call_id, arguments, expected_type_arguments)?
                else {
                    return Err(SemanticError::WrongArgumentCount {
                        function: if is_set { "Set.empty" } else { "Map.empty" }.to_owned(),
                        expected: 0,
                        span: call_span,
                    });
                };
                if type_values.len() != expected_type_arguments {
                    return Err(SemanticError::WrongArgumentCount {
                        function: if is_set { "Set.empty" } else { "Map.empty" }.to_owned(),
                        expected: 0,
                        span: call_span,
                    });
                }
                let key = type_values[0];
                let value = if is_set { Type::Bool } else { type_values[1] };
                self.check_map_key(key, call_span)?;
                if !is_set {
                    self.check_list_element(value, call_span)?;
                }
                return Ok(Type::Map(intern_map(&mut self.maps, key, value)));
            }
            if let Some(enum_type) = self.resolve_builtin_enum_type_value(value)? {
                let variants = match enum_type {
                    Type::Option(id) => vec![
                        (
                            "Some".to_owned(),
                            vec![("value".to_owned(), self.option_types[id])],
                        ),
                        ("None".to_owned(), Vec::new()),
                    ],
                    Type::Result(id) => {
                        let (ok, err) = self.result_types[id];
                        vec![
                            ("Ok".to_owned(), vec![("value".to_owned(), ok)]),
                            ("Err".to_owned(), vec![("value".to_owned(), err)]),
                        ]
                    }
                    Type::Enum(enum_id) => self
                        .enums
                        .values()
                        .find(|enumeration| enumeration.id == enum_id)
                        .map(|enumeration| {
                            enumeration
                                .variants
                                .iter()
                                .map(|variant| (variant.name.clone(), variant.fields.clone()))
                                .collect()
                        })
                        .unwrap_or_default(),
                    _ => Vec::new(),
                };
                if let Some((_, fields)) = variants.iter().find(|(name, _)| name == variant_name) {
                    if fields.is_empty() {
                        return Err(SemanticError::NotCallable { span: callee.span });
                    }
                    for (argument, (_, expected_type)) in self
                        .order_arguments(
                            arguments,
                            fields,
                            &format!("{}.{}", crate::sema::type_name(enum_type), variant_name),
                            call_span,
                        )?
                        .into_iter()
                        .zip(fields)
                    {
                        self.check_expression(argument, TypeExpectation::require(*expected_type))?;
                    }
                    return Ok(enum_type);
                } else {
                    return Err(SemanticError::UnknownEnumVariant {
                        enum_name: crate::sema::type_name(enum_type),
                        variant: variant_name.clone(),
                        span: callee.span,
                    });
                }
            }
            if let Some(enum_id) = self.resolve_enum_type_value(value)? {
                if let Some(enumeration) = self
                    .enums
                    .values()
                    .find(|enumeration| enumeration.id == enum_id)
                    .cloned()
                {
                    let variant = enumeration
                        .variants
                        .iter()
                        .find(|variant| variant.name == *variant_name)
                        .ok_or_else(|| SemanticError::UnknownEnumVariant {
                            enum_name: format!("enum_{enum_id}"),
                            variant: variant_name.clone(),
                            span: callee.span,
                        })?;
                    if variant.fields.is_empty() {
                        return Err(SemanticError::NotCallable { span: callee.span });
                    }
                    for (argument, (_, expected_type)) in self
                        .order_arguments(
                            arguments,
                            &variant.fields,
                            &format!("enum_{enum_id}.{variant_name}"),
                            call_span,
                        )?
                        .into_iter()
                        .zip(&variant.fields)
                    {
                        self.check_expression(argument, TypeExpectation::require(*expected_type))?;
                    }
                    return Ok(Type::Enum(enumeration.id));
                }
            }
        }
        if matches!(&callee.kind, ExprKind::Field { .. }) {
            return self.check_method_call(callee, arguments, call_span, call_id, expectation);
        }

        if let Ok(Type::Function(id)) = self.check_expression(callee, TypeExpectation::none()) {
            let signature = self
                .function_types
                .get(id)
                .cloned()
                .ok_or(SemanticError::NotCallable { span: callee.span })?;
            if arguments.len() != signature.parameters.len() {
                return Err(SemanticError::WrongArgumentCount {
                    function: "closure".to_owned(),
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
                .order_arguments(arguments, &parameters, "closure", call_span)?
                .into_iter()
                .zip(&parameters)
            {
                self.check_expression(argument, TypeExpectation::require(*expected_type))?;
            }
            self.current_effects.extend(&signature.effects);
            return Ok(signature.return_type);
        }

        let ExprKind::Name(name) = &callee.kind else {
            return Err(SemanticError::NotCallable { span: callee.span });
        };

        if let Some(prefix) = self
            .current_module_prefix
            .as_ref()
            .filter(|p| p.starts_with("@prelude/"))
        {
            let qualified = format!("{prefix}{name}");
            if self.functions.contains_key(&qualified) {
                let callee = Expr {
                    kind: ExprKind::Name(qualified),
                    ..callee.clone()
                };
                return self.check_call(&callee, arguments, call_span, call_id, expectation);
            }
        }

        if let Some(structure) = self.structs.get(name).cloned() {
            let parameters = structure.fields.clone();
            let defaults = structure.defaults.clone();
            for (argument, (_, expected_type)) in self
                .order_arguments_with_defaults(arguments, &parameters, &defaults, name, call_span)?
                .into_iter()
                .zip(&parameters)
                .filter_map(|(argument, parameter)| argument.map(|argument| (argument, parameter)))
            {
                self.check_expression(argument, TypeExpectation::require(*expected_type))?;
            }
            return Ok(Type::Struct(structure.id));
        }
        if let Some(class) = self.classes.get(name).cloned() {
            let parameters = class
                .fields
                .iter()
                .map(|(field_name, field, _)| (field_name.clone(), field.ty))
                .collect::<Vec<_>>();
            let defaults = class
                .fields
                .iter()
                .map(|(_, _, default)| default.clone())
                .collect::<Vec<_>>();
            for (argument, (_, expected_type)) in self
                .order_arguments_with_defaults(arguments, &parameters, &defaults, name, call_span)?
                .into_iter()
                .zip(&parameters)
                .filter_map(|(argument, parameter)| argument.map(|argument| (argument, parameter)))
            {
                self.check_expression(argument, TypeExpectation::require(*expected_type))?;
            }
            return Ok(Type::Class(class.id));
        }

        if let Some(op) = super::path_intrinsics::operation(name) {
            // Intern all result shapes before the shared signature is read.
            super::checker::intern_option(&mut self.option_types, Type::String);
            super::checker::intern_list(&mut self.list_types, Type::String);
            super::checker::intern_result(&mut self.result_types, Type::String, Type::String);
            let expected_count = if matches!(op, 0 | 6 | 7) { 2 } else { 1 };
            if arguments.len() != expected_count {
                return Err(SemanticError::WrongArgumentCount {
                    function: name.clone(),
                    expected: expected_count,
                    span: call_span,
                });
            }
            for argument in arguments {
                let ty = if op == 9 {
                    Type::List(intern_list(&mut self.list_types, Type::String))
                } else {
                    Type::String
                };
                self.check_expression(&argument.value, TypeExpectation::require(ty))?;
            }
            return Ok(match op {
                0 | 6 => Type::String,
                1 => Type::Bool,
                2..=5 | 7 => Type::Option(super::checker::intern_option(
                    &mut self.option_types,
                    Type::String,
                )),
                8 => Type::List(intern_list(&mut self.list_types, Type::String)),
                9 => Type::Result(super::checker::intern_result(
                    &mut self.result_types,
                    Type::String,
                    Type::String,
                )),
                _ => unreachable!(),
            });
        }

        // Check built-in functions
        if name == "echo" {
            // The parser supplies the second argument: the original source location.
            if arguments.len() != 2 {
                return Err(SemanticError::WrongArgumentCount {
                    function: "echo".into(),
                    expected: 1,
                    span: call_span,
                });
            }
            let ty = self.check_expression(&arguments[0].value, expectation)?;
            self.check_expression(&arguments[1].value, TypeExpectation::require(Type::String))?;
            if !self.implements_debug(ty) {
                return Err(SemanticError::TraitBoundNotSatisfied {
                    trait_name: "Debug".into(),
                    actual: super::type_name(ty),
                    span: arguments[0].value.span,
                });
            }
            return Ok(ty);
        }
        if matches!(name.as_str(), "print" | "println") {
            if arguments.len() != 1 {
                return Err(SemanticError::WrongArgumentCount {
                    function: name.to_owned(),
                    expected: 1,
                    span: call_span,
                });
            }
            let argument_type =
                self.check_expression(&arguments[0].value, TypeExpectation::none())?;
            if !self.implements_show(argument_type) {
                return Err(SemanticError::TraitBoundNotSatisfied {
                    trait_name: "Show".to_owned(),
                    actual: super::type_name(argument_type),
                    span: arguments[0].value.span,
                });
            }
            return Ok(Type::Unit);
        }

        if name == "panic" {
            if arguments.len() != 1 {
                return Err(SemanticError::WrongArgumentCount {
                    function: "panic".to_owned(),
                    expected: 1,
                    span: call_span,
                });
            }
            self.check_expression(&arguments[0].value, TypeExpectation::require(Type::String))?;
            return Ok(Type::Unit);
        }

        // Check user-defined functions
        let signature =
            self.functions
                .get(name)
                .cloned()
                .ok_or_else(|| SemanticError::UnknownFunction {
                    name: name.clone(),
                    span: callee.span,
                })?;

        // The unified spelling `f(T, value)` passes the type values in the
        // source argument list, but they are consumed entirely at compile
        // time and must not reach the runtime ABI.
        let compile_time_arguments = if signature.type_parameters.is_empty() {
            0
        } else if arguments.len() == signature.parameters.len() + signature.type_parameters.len() {
            signature.type_parameters.len()
        } else {
            0
        };
        if arguments.len() != signature.parameters.len() + compile_time_arguments {
            return Err(SemanticError::WrongArgumentCount {
                function: name.clone(),
                expected: signature.parameters.len(),
                span: call_span,
            });
        }

        let runtime_arguments = &arguments[compile_time_arguments..];
        let ordered =
            self.order_arguments(runtime_arguments, &signature.parameters, name, call_span)?;
        if signature.type_parameters.is_empty() {
            let foreign = self.foreign_functions.contains(name);
            for (arg, (_, expected_type)) in ordered.iter().zip(&signature.parameters) {
                self.check_expression(arg, TypeExpectation::require(*expected_type))?;
                if foreign {
                    if let Type::Function(_) = expected_type {
                        self.require_zero_capture_callback(arg)?;
                    }
                }
            }
            self.current_effects.extend(&signature.used_effects);
            return Ok(signature.return_type);
        }

        let mut substitutions = vec![None; signature.type_parameters.len()];
        if compile_time_arguments > 0 {
            for (index, (slot, argument)) in substitutions
                .iter_mut()
                .zip(&arguments[..compile_time_arguments])
                .enumerate()
            {
                if argument
                    .label
                    .as_ref()
                    .is_some_and(|label| label != &signature.type_parameters[index])
                {
                    return Err(SemanticError::UnknownFunction {
                        name: argument.label.clone().unwrap(),
                        span: argument.value.span,
                    });
                }
                *slot = Some(
                    self.resolve_type_value_expr(&argument.value)?
                        .ok_or_else(|| SemanticError::UnknownType {
                            name: "expression is not a compile-time type value".to_owned(),
                            span: argument.value.span,
                        })?,
                );
            }
        }
        if let Some(expected) = expectation.ty() {
            self.unify_generic_type(
                signature.return_type,
                expected,
                &mut substitutions,
                call_span,
            )?;
        }
        for (argument, (_, template)) in ordered.iter().zip(&signature.parameters) {
            if let Some(actual) =
                self.inferred_closure_argument(argument, *template, &substitutions)?
            {
                self.unify_generic_type(*template, actual, &mut substitutions, argument.span)?;
                continue;
            }
            let hint = self
                .substitute_type(*template, &substitutions)
                .map(TypeExpectation::require)
                .unwrap_or_else(TypeExpectation::none);
            if hint.ty().is_none() && can_defer_generic_argument(argument) {
                continue;
            }
            let actual = self.check_expression(argument, hint)?;
            self.unify_generic_type(*template, actual, &mut substitutions, argument.span)?;
        }
        let concrete = substitutions
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| SemanticError::UnknownFunction {
                name: format!("cannot infer all type arguments for '{name}'"),
                span: call_span,
            })?;
        for (bounds, actual) in signature.type_parameter_bounds.iter().zip(&concrete) {
            for bound in bounds {
                if !self.implements_trait(*actual, bound) {
                    return Err(SemanticError::TraitBoundNotSatisfied {
                        trait_name: bound.clone(),
                        actual: super::type_name(*actual),
                        span: call_span,
                    });
                }
            }
        }
        let substitutions = concrete.iter().copied().map(Some).collect::<Vec<_>>();
        self.check_associated_bounds(&signature.associated_bounds, &substitutions, call_span)?;
        for (argument, (_, template)) in ordered.into_iter().zip(&signature.parameters) {
            let expected = self
                .substitute_type(
                    *template,
                    &concrete.iter().copied().map(Some).collect::<Vec<_>>(),
                )
                .expect("all generic parameters were inferred");
            self.check_list_types(expected, argument.span)?;
            self.check_expression(argument, TypeExpectation::require(expected))?;
        }
        let return_type = self
            .substitute_type(signature.return_type, &substitutions)
            .expect("all return type parameters were inferred");
        self.current_effects.extend(&signature.used_effects);
        self.check_list_types(return_type, call_span)?;
        self.generic_call_spans.insert(call_id, call_span);
        self.generic_calls.insert(
            call_id,
            super::GenericCallInstance {
                function: name.clone(),
                arguments: concrete,
                compile_time_arguments,
            },
        );
        Ok(return_type)
    }
}

fn can_defer_generic_argument(argument: &Expr) -> bool {
    matches!(&argument.kind, ExprKind::Name(name) if name == "None")
}

impl Checker {
    /// A closure passed to `fn(T) -> U` can be checked once `T` is known.
    /// Its body supplies `U`, which is not available as a complete function
    /// type until the closure has been checked.
    fn inferred_closure_argument(
        &mut self,
        argument: &Expr,
        template: Type,
        substitutions: &[Option<Type>],
    ) -> Result<Option<Type>, SemanticError> {
        let Type::Function(template_id) = template else {
            return Ok(None);
        };
        let ExprKind::Closure {
            move_capture,
            parameters,
            return_type,
            body,
        } = &argument.kind
        else {
            return Ok(None);
        };
        if self.substitute_type(template, substitutions).is_some() {
            return Ok(None);
        }
        let info = self.function_types[template_id].clone();
        if info.parameters.len() != parameters.len() {
            return Ok(None);
        }
        let Some(concrete_parameters) = info
            .parameters
            .iter()
            .map(|ty| self.substitute_type(*ty, substitutions))
            .collect::<Option<Vec<_>>>()
        else {
            return Ok(None);
        };
        let hint = Type::Function(super::checker::intern_function(
            &mut self.function_types,
            info.parameter_names,
            concrete_parameters,
            Type::Unit,
        ));
        let actual = self.check_closure(
            *move_capture,
            parameters,
            return_type,
            body,
            argument.span,
            argument.id,
            TypeExpectation::require(hint),
            true,
        )?;
        Ok(Some(actual))
    }
}
