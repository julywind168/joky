//! Compile-time type computation. Type functions have their own lexical
//! environment and never produce runtime expressions or call instances.

use std::collections::{HashMap, HashSet};

use crate::diagnostic::SemanticError;
use crate::syntax::{Expr, ExprKind, Function};
use crate::Span;

use super::checker::{
    intern_list, intern_map, intern_option, intern_result, intern_tuple, Checker,
};
use super::expectation::TypeExpectation;
use super::symbol_table::{EnumInfo, EnumVariantInfo, StructInfo};
use super::types::{primitive_type, Type};

type Environment = HashMap<String, Option<Type>>;

fn expected_type(span: Span) -> SemanticError {
    SemanticError::UnknownType {
        name: "expected a compile-time type value".to_owned(),
        span,
    }
}

fn constructor_arity(name: &str) -> Option<usize> {
    match name {
        "Cown" | "CPtr" | "CMutPtr" | "List" | "MutList" | "Option" | "Set" | "MutSet"
        | "MutListCursor" | "MutSetCursor" | "Range" => Some(1),
        "Map" | "MutMap" | "Result" | "MapCursor" | "MapKeyCursor" | "MapValueCursor"
        | "MutMapCursor" => Some(2),
        _ => None,
    }
}

fn primitive_name(ty: Type) -> Option<&'static str> {
    Some(match ty {
        Type::I8 => "Int8",
        Type::I16 => "Int16",
        Type::I32 => "Int32",
        Type::I64 => "Int64",
        Type::U8 => "UInt8",
        Type::U16 => "UInt16",
        Type::U32 => "UInt32",
        Type::U64 => "UInt64",
        Type::F32 => "Float32",
        Type::F64 => "Float64",
        Type::String => "String",
        Type::Bytes => "Bytes",
        Type::MutBytes => "MutBytes",
        Type::Bool => "Bool",
        Type::Duration => "Duration",
        Type::Unit => "Unit",
        _ => return None,
    })
}

impl Checker {
    pub(super) fn resolve_builtin_enum_type_value(
        &mut self,
        expression: &Expr,
    ) -> Result<Option<Type>, SemanticError> {
        let value = self.resolve_type_value_expr(expression)?;
        if matches!(
            value,
            Some(Type::Option(_) | Type::Result(_) | Type::Enum(_))
        ) {
            if let Some(value) = value {
                self.types.insert(expression.id, value);
                return Ok(Some(value));
            }
        }
        Ok(None)
    }

    pub(super) fn resolve_enum_type_value(
        &mut self,
        expression: &Expr,
    ) -> Result<Option<usize>, SemanticError> {
        let Some(Type::Enum(id)) = self.resolve_type_value_expr(expression)? else {
            return Ok(None);
        };
        self.types.insert(expression.id, Type::Enum(id));
        Ok(Some(id))
    }

    pub(super) fn resolve_type_value_expr(
        &mut self,
        expression: &Expr,
    ) -> Result<Option<Type>, SemanticError> {
        let mut environment = self
            .current_type_parameters
            .iter()
            .enumerate()
            .map(|(id, name)| (name.clone(), Some(Type::Param(id))))
            .collect::<Environment>();
        for scope in &self.scopes {
            for (name, binding) in scope {
                environment.insert(name.clone(), binding.type_value);
            }
        }
        let value = self.evaluate_type_expression(expression, &environment, &mut Vec::new())?;
        if let Some(ty) = value {
            self.check_list_types(ty, expression.span)?;
        }
        Ok(value)
    }

    pub(super) fn validate_type_function(
        &mut self,
        function: &Function,
    ) -> Result<(), SemanticError> {
        let environment = function
            .type_parameters
            .iter()
            .enumerate()
            .map(|(id, parameter)| (parameter.name.clone(), Some(Type::Param(id))))
            .collect();
        // Symbolic map keys may accumulate constraints while checking the
        // body. Do not leak these parameter indices into the caller.
        let previous_bounds = std::mem::replace(
            &mut self.current_type_parameter_bounds,
            vec![Vec::new(); function.type_parameters.len()],
        );
        let result = self.evaluate_type_expression(
            &function.body,
            &environment,
            &mut vec![function.name.clone()],
        );
        self.current_type_parameter_bounds = previous_bounds;
        result?.ok_or_else(|| expected_type(function.body.span))?;
        Ok(())
    }

    pub(super) fn register_type_function(
        &mut self,
        function: &Function,
    ) -> Result<(), SemanticError> {
        if function.name == "main"
            || !function.parameters.is_empty()
            || !function.effect_names.is_empty()
            || function
                .type_parameters
                .iter()
                .any(|p| !p.bounds.is_empty())
        {
            return Err(SemanticError::FunctionNotSupported {
                name: "type functions require only unconstrained type parameters, no effects, and cannot be main"
                    .to_owned(),
                span: function.span,
            });
        }
        if self.type_functions.contains_key(&function.name)
            || self.functions.contains_key(&function.name)
            || self.constants.contains_key(&function.name)
            || self.structs.contains_key(&function.name)
            || self.classes.contains_key(&function.name)
            || self.enums.contains_key(&function.name)
            || primitive_type(&function.name).is_some()
            || (constructor_arity(&function.name).is_some()
                && !super::prelude::canonical_type_definition(function))
            || function.name == "Dyn"
        {
            return Err(SemanticError::DuplicateFunctionDefinition {
                name: function.name.clone(),
                span: function.span,
            });
        }
        let mut names = HashSet::new();
        for parameter in &function.type_parameters {
            if !names.insert(&parameter.name) {
                return Err(SemanticError::DuplicateTypeParameter {
                    name: parameter.name.clone(),
                    span: parameter.span,
                });
            }
        }
        self.type_functions
            .insert(function.name.clone(), function.clone());
        Ok(())
    }

    fn evaluate_imported_type_name(
        &mut self,
        value: &Expr,
        name: &str,
        environment: &Environment,
        span: Span,
    ) -> Result<Option<Type>, SemanticError> {
        let ExprKind::Name(alias) = &value.kind else {
            return Ok(None);
        };
        if environment.contains_key(alias) {
            return Ok(None);
        }
        let Some(module) = self.import_aliases.get(alias).copied() else {
            return Ok(None);
        };
        let Some(table) = self.dependency_types.get(&module).cloned() else {
            return Ok(None);
        };
        let qualified = format!("@{:016x}/{name}", module.0);
        let nominal = |name: &str| {
            table
                .struct_type(name)
                .or_else(|| table.class_type(name))
                .or_else(|| table.enum_type(name))
        };
        nominal(&qualified)
            .or_else(|| nominal(name))
            .map(|ty| self.import_abi_type(module, &table, ty, span))
            .transpose()
    }

    fn evaluate_type_expression(
        &mut self,
        expression: &Expr,
        environment: &Environment,
        stack: &mut Vec<String>,
    ) -> Result<Option<Type>, SemanticError> {
        // Keep block frames small so the declared depth limit is reached before
        // exhausting the host stack in unoptimized compiler builds.
        if let ExprKind::Block(expressions) = &expression.kind {
            if !stack.is_empty() {
                let Some((last, preceding)) = expressions.split_last() else {
                    return Err(expected_type(expression.span));
                };
                let mut local = environment.clone();
                for expression in preceding {
                    let ExprKind::Let {
                        name,
                        annotation,
                        value,
                        mutable,
                    } = &expression.kind
                    else {
                        return Err(expected_type(expression.span));
                    };
                    if *mutable {
                        return Err(SemanticError::MutableTypeBinding {
                            name: name.clone(),
                            span: expression.span,
                        });
                    }
                    if annotation.as_ref().is_some_and(|ty| !ty.is_name("type")) {
                        return Err(expected_type(expression.span));
                    }
                    let value = self
                        .evaluate_type_expression(value, &local, stack)?
                        .ok_or_else(|| expected_type(value.span))?;
                    local.insert(name.clone(), Some(value));
                }
                return self.evaluate_type_expression(last, &local, stack);
            }
        }
        self.evaluate_type_expression_value(expression, environment, stack)
    }

    fn evaluate_type_expression_value(
        &mut self,
        expression: &Expr,
        environment: &Environment,
        stack: &mut Vec<String>,
    ) -> Result<Option<Type>, SemanticError> {
        match &expression.kind {
            ExprKind::Name(name) => {
                if let Some(value) = environment.get(name) {
                    return Ok(*value);
                }
                Ok(self
                    .type_bindings
                    .get(name)
                    .copied()
                    .or_else(|| primitive_type(name))
                    .or_else(|| self.structs.get(name).map(|info| Type::Struct(info.id)))
                    .or_else(|| self.classes.get(name).map(|info| Type::Class(info.id)))
                    .or_else(|| self.enums.get(name).map(|info| Type::Enum(info.id))))
            }
            ExprKind::Field {
                value,
                access: crate::syntax::FieldAccess::Name(name),
            } => self.evaluate_imported_type_name(value, name, environment, expression.span),
            ExprKind::Call { callee, arguments } => {
                if let ExprKind::Field {
                    value,
                    access: crate::syntax::FieldAccess::Name(name),
                } = &callee.kind
                {
                    if let ExprKind::Name(alias) = &value.kind {
                        if let Some(module) = self.import_aliases.get(alias).copied() {
                            let is_type_function = self
                                .dependency_types
                                .get(&module)
                                .and_then(|table| table.interface.type_program.as_ref())
                                .is_some_and(|program| {
                                    program.functions.iter().any(|f| {
                                        f.name == *name
                                            && f.return_type
                                                .as_ref()
                                                .is_some_and(|ty| ty.is_name("type"))
                                    })
                                });
                            if is_type_function {
                                let mut values = Vec::new();
                                for arg in arguments {
                                    let ty = self
                                        .evaluate_type_expression(&arg.value, environment, stack)?
                                        .ok_or_else(|| expected_type(arg.value.span))?;
                                    values.push((arg.label.clone(), ty, arg.value.span));
                                }
                                return self
                                    .import_type_function(module, name, &values, expression.span)
                                    .map(Some);
                            }
                        }
                    }
                }
                let ExprKind::Name(name) = &callee.kind else {
                    return Ok(None);
                };
                if environment.contains_key(name) {
                    return Ok(None);
                }
                if name == "Dyn" {
                    let first = arguments
                        .first()
                        .filter(|arg| arg.label.is_none())
                        .ok_or_else(|| {
                            super::dynamic::invalid("Dyn requires a trait name", expression.span)
                        })?;
                    let trait_names = super::dynamic::trait_names_from_expression(&first.value)?;
                    let mut bindings = Vec::new();
                    for argument in &arguments[1..] {
                        let ty = self
                            .evaluate_type_expression(&argument.value, environment, stack)?
                            .ok_or_else(|| expected_type(argument.value.span))?;
                        bindings.push((argument.label.clone(), ty));
                    }
                    return self
                        .resolve_dynamic_type(&trait_names, bindings, expression.span)
                        .map(Some);
                }
                if name == "CArray" {
                    if arguments.len() != 2 || arguments.iter().any(|arg| arg.label.is_some()) {
                        return Err(SemanticError::FunctionNotSupported {
                            name: "CArray requires an element type and a positive integer constant"
                                .into(),
                            span: expression.span,
                        });
                    }
                    let element = self
                        .evaluate_type_expression(&arguments[0].value, environment, stack)?
                        .ok_or_else(|| expected_type(arguments[0].value.span))?;
                    let ExprKind::Integer(count) = arguments[1].value.kind else {
                        return Err(SemanticError::FunctionNotSupported {
                            name: "CArray length must be an unsuffixed integer constant".into(),
                            span: arguments[1].value.span,
                        });
                    };
                    if count == 0 {
                        return Err(SemanticError::FunctionNotSupported {
                            name: "CArray length must be greater than zero".into(),
                            span: expression.span,
                        });
                    }
                    let id = self
                        .c_arrays
                        .iter()
                        .position(|entry| *entry == (element, count))
                        .unwrap_or_else(|| {
                            let id = self.c_arrays.len();
                            self.c_arrays.push((element, count));
                            id
                        });
                    return Ok(Some(Type::CArray(id)));
                }
                let function = self.type_functions.get(name).cloned();
                let Some(_) = function
                    .as_ref()
                    .map(|f| f.type_parameters.len())
                    .or_else(|| self.intrinsic_type_arities.get(name).copied())
                    .or_else(|| constructor_arity(name))
                else {
                    return Ok(None);
                };
                let values = arguments
                    .iter()
                    .map(|arg| self.evaluate_type_expression(&arg.value, environment, stack))
                    .collect::<Result<Option<Vec<_>>, _>>()?;
                let Some(values) = values else {
                    return if function.is_some() {
                        Err(expected_type(expression.span))
                    } else {
                        // Ordinary value constructors remain on the runtime path.
                        Ok(None)
                    };
                };
                let arguments = arguments
                    .iter()
                    .zip(values)
                    .map(|(argument, value)| (argument.label.clone(), value, argument.value.span))
                    .collect::<Vec<_>>();
                self.apply_type_value(name, &arguments, expression.span, stack)
                    .map(Some)
            }
            ExprKind::AnonymousStruct { fields } if !stack.is_empty() => {
                let mut resolved = Vec::with_capacity(fields.len());
                let mut defaults = Vec::with_capacity(fields.len());
                for field in fields {
                    let ty = self
                        .resolve_type_in_environment(&field.ty, environment, stack)?
                        .ok_or_else(|| expected_type(field.ty.span))?;
                    resolved.push((field.name.clone(), ty));
                    let default = field
                        .default
                        .as_ref()
                        .map(|default| self.specialize_type_value_expression(default, environment));
                    let symbolic = environment
                        .values()
                        .flatten()
                        .any(|value| matches!(value, Type::Param(_)));
                    if !symbolic {
                        if let Some(default) = &default {
                            self.check_expression(default, TypeExpectation::require(ty))?;
                        }
                    }
                    defaults.push(default);
                }
                let function = stack
                    .last()
                    .cloned()
                    .unwrap_or_else(|| "<anonymous>".to_owned());
                let key = (
                    function.clone(),
                    expression.id,
                    resolved.iter().map(|(_, ty)| *ty).collect(),
                );
                if let Some(id) = self.generated_structs.get(&key) {
                    return Ok(Some(Type::Struct(*id)));
                }
                let id = self.structs.len();
                let name = self.generated_type_name(&function, "struct", expression.id, &key.2);
                self.structs.insert(
                    name,
                    StructInfo {
                        repr_c: false,
                        id,
                        fields: resolved,
                        defaults,
                        methods: HashMap::new(),
                    },
                );
                self.generated_structs.insert(key, id);
                Ok(Some(Type::Struct(id)))
            }
            ExprKind::AnonymousEnum { variants } if !stack.is_empty() => {
                let mut resolved_variants = Vec::with_capacity(variants.len());
                let mut key_types = Vec::new();
                for variant in variants {
                    let mut fields = Vec::with_capacity(variant.fields.len());
                    for field in &variant.fields {
                        let ty = self
                            .resolve_type_in_environment(&field.ty, environment, stack)?
                            .ok_or_else(|| expected_type(field.ty.span))?;
                        fields.push((field.name.clone(), ty));
                        key_types.push(ty);
                    }
                    resolved_variants.push(EnumVariantInfo {
                        name: variant.name.clone(),
                        fields,
                    });
                }
                let function = stack.last().cloned().unwrap_or_default();
                // These source enum definitions retain the runtime's canonical ABI.
                match function.strip_prefix("@prelude/").unwrap_or(&function) {
                    "Option" => {
                        return Ok(Some(Type::Option(intern_option(
                            &mut self.option_types,
                            resolved_variants[0].fields[0].1,
                        ))))
                    }
                    "Result" => {
                        return Ok(Some(Type::Result(intern_result(
                            &mut self.result_types,
                            resolved_variants[0].fields[0].1,
                            resolved_variants[1].fields[0].1,
                        ))))
                    }
                    _ => {}
                }
                let key = (function.clone(), expression.id, key_types);
                if let Some(id) = self.generated_enums.get(&key) {
                    return Ok(Some(Type::Enum(*id)));
                }
                let id = self.enums.len();
                let name = self.generated_type_name(&function, "enum", expression.id, &key.2);
                self.enums.insert(
                    name,
                    EnumInfo {
                        id,
                        variants: resolved_variants,
                    },
                );
                self.generated_enums.insert(key, id);
                Ok(Some(Type::Enum(id)))
            }
            _ => Ok(None),
        }
    }

    fn resolve_type_in_environment(
        &mut self,
        annotation: &crate::syntax::TypeAnnotation,
        environment: &Environment,
        stack: &mut Vec<String>,
    ) -> Result<Option<Type>, SemanticError> {
        match &annotation.kind {
            crate::syntax::TypeExpr::Name(name) => {
                Ok(environment.get(name).copied().flatten().or_else(|| {
                    primitive_type(name)
                        .or_else(|| self.structs.get(name).map(|info| Type::Struct(info.id)))
                        .or_else(|| self.classes.get(name).map(|info| Type::Class(info.id)))
                        .or_else(|| self.enums.get(name).map(|info| Type::Enum(info.id)))
                }))
            }
            crate::syntax::TypeExpr::Apply { callee, arguments } => {
                let crate::syntax::TypeExpr::Name(name) = &callee.kind else {
                    return Ok(None);
                };
                if name == "Dyn" {
                    let first = arguments
                        .first()
                        .filter(|arg| arg.label.is_none())
                        .ok_or_else(|| {
                            super::dynamic::invalid("Dyn requires a trait name", annotation.span)
                        })?;
                    let mut bindings = Vec::new();
                    for argument in &arguments[1..] {
                        let ty = self
                            .resolve_type_in_environment(&argument.value, environment, stack)?
                            .ok_or_else(|| expected_type(argument.value.span))?;
                        bindings.push((argument.label.clone(), ty));
                    }
                    let names = super::dynamic::trait_names_from_annotation(&first.value)?;
                    return self
                        .resolve_dynamic_type(&names, bindings, annotation.span)
                        .map(Some);
                }
                let values = arguments
                    .iter()
                    .map(|argument| {
                        self.resolve_type_in_environment(&argument.value, environment, stack)
                    })
                    .collect::<Result<Option<Vec<_>>, _>>()?;
                let Some(values) = values else {
                    return Ok(None);
                };
                let args = arguments
                    .iter()
                    .zip(values)
                    .map(|(argument, value)| (argument.label.clone(), value, argument.value.span))
                    .collect::<Vec<_>>();
                self.apply_type_value(name, &args, annotation.span, stack)
                    .map(Some)
            }
            _ => Ok(None),
        }
    }

    fn generated_type_name(
        &self,
        function: &str,
        kind: &str,
        node: crate::syntax::NodeId,
        arguments: &[Type],
    ) -> String {
        let table = super::ModuleTypes::snapshot(self);
        let module = self.definition_module.unwrap_or(crate::module::StableId(0));
        let arguments = arguments
            .iter()
            .map(|ty| table.stable_type_key(*ty, module))
            .collect::<Vec<_>>();
        let payload = bincode::serialize(&(function, kind, node, arguments))
            .expect("serializable generated type identity");
        format!(
            "{function}${kind}{:016x}",
            crate::module::source_fingerprint(&payload)
        )
    }

    fn specialize_type_value_expression(
        &self,
        expression: &Expr,
        environment: &Environment,
    ) -> Expr {
        let mut specialized = expression.clone();
        match &mut specialized.kind {
            ExprKind::Name(name) => {
                if let Some(Some(ty)) = environment.get(name) {
                    if let Some(canonical) = primitive_name(*ty) {
                        *name = canonical.to_owned();
                    }
                }
            }
            ExprKind::Call {
                callee, arguments, ..
            } => {
                **callee = self.specialize_type_value_expression(callee, environment);
                for argument in arguments {
                    argument.value =
                        self.specialize_type_value_expression(&argument.value, environment);
                }
            }
            ExprKind::Tuple(elements) | ExprKind::Block(elements) => {
                for element in elements {
                    *element = self.specialize_type_value_expression(element, environment);
                }
            }
            _ => {}
        }
        specialized
    }

    pub(super) fn apply_type_value(
        &mut self,
        name: &str,
        arguments: &[(Option<String>, Type, Span)],
        span: Span,
        stack: &mut Vec<String>,
    ) -> Result<Type, SemanticError> {
        let function = self
            .type_functions
            .get(name)
            .or_else(|| self.type_functions.get(&format!("@prelude/{name}")))
            .cloned();
        let arity = function
            .as_ref()
            .map(|f| f.type_parameters.len())
            .or_else(|| self.intrinsic_type_arities.get(name).copied())
            .or_else(|| constructor_arity(name))
            .ok_or_else(|| SemanticError::UnknownType {
                name: name.to_owned(),
                span,
            })?;
        if arguments.len() != arity {
            return Err(SemanticError::WrongArgumentCount {
                function: name.to_owned(),
                expected: arity,
                span,
            });
        }
        if let Some(function) = function {
            if stack.iter().any(|item| item == name) || stack.len() >= 64 {
                return Err(SemanticError::FunctionNotSupported {
                    name: "recursive type functions or type evaluation depth exceeding 64"
                        .to_owned(),
                    span,
                });
            }
            let mut ordered = vec![None; arity];
            for (index, (label, value, span)) in arguments.iter().enumerate() {
                let index = if let Some(label) = label {
                    function
                        .type_parameters
                        .iter()
                        .position(|p| &p.name == label)
                        .ok_or_else(|| expected_type(*span))?
                } else {
                    index
                };
                if ordered[index].replace(*value).is_some() {
                    return Err(expected_type(*span));
                }
            }
            let local = function
                .type_parameters
                .iter()
                .zip(ordered)
                .map(|(p, value)| (p.name.clone(), value))
                .collect();
            stack.push(function.name.clone());
            let result = self.evaluate_type_expression(&function.body, &local, stack);
            stack.pop();
            return result?.ok_or_else(|| expected_type(function.body.span));
        }
        if arguments.iter().any(|(label, _, _)| label.is_some()) {
            return Err(SemanticError::FunctionNotSupported {
                name: "builtin type constructors require positional arguments".to_owned(),
                span,
            });
        }
        let first = arguments[0].1;
        let ty = match name {
            "CPtr" | "CMutPtr" => {
                let id = self
                    .c_pointers
                    .iter()
                    .position(|ty| *ty == first)
                    .unwrap_or_else(|| {
                        let id = self.c_pointers.len();
                        self.c_pointers.push(first);
                        id
                    });
                if name == "CPtr" {
                    Type::CPtr(id)
                } else {
                    Type::CMutPtr(id)
                }
            }
            "Cown" => Type::Cown(super::checker::intern_cown(&mut self.cowns, first)),
            "Range" => self.intern_range(first, span)?,
            "List" => Type::List(intern_list(&mut self.list_types, first)),
            "MutList" => Type::MutList(intern_list(&mut self.list_types, first)),
            "MutListCursor" => {
                let cursor = Type::MutListCursor(intern_list(&mut self.list_types, first));
                intern_option(
                    &mut self.option_types,
                    Type::Tuple(intern_tuple(&mut self.tuple_types, vec![first, cursor])),
                );
                cursor
            }
            "Map" => Type::Map(intern_map(&mut self.maps, first, arguments[1].1)),
            "MapCursor" => {
                let _ = intern_tuple(&mut self.tuple_types, vec![first, arguments[1].1]);
                Type::MapCursor(intern_map(&mut self.maps, first, arguments[1].1))
            }
            "MapKeyCursor" => {
                let _ = intern_tuple(&mut self.tuple_types, vec![first, arguments[1].1]);
                Type::MapKeyCursor(intern_map(&mut self.maps, first, arguments[1].1))
            }
            "MapValueCursor" => {
                let _ = intern_tuple(&mut self.tuple_types, vec![first, arguments[1].1]);
                Type::MapValueCursor(intern_map(&mut self.maps, first, arguments[1].1))
            }
            "MutMap" => Type::MutMap(intern_map(&mut self.maps, first, arguments[1].1)),
            "MutMapCursor" => {
                let _ = intern_tuple(&mut self.tuple_types, vec![first, arguments[1].1]);
                let cursor = Type::MutMapCursor(intern_map(&mut self.maps, first, arguments[1].1));
                let item = Type::Tuple(intern_tuple(
                    &mut self.tuple_types,
                    vec![first, arguments[1].1],
                ));
                intern_option(
                    &mut self.option_types,
                    Type::Tuple(intern_tuple(&mut self.tuple_types, vec![item, cursor])),
                );
                cursor
            }
            "Set" => Type::Map(intern_map(&mut self.maps, first, Type::Bool)),
            "MutSet" => Type::MutSet(intern_map(&mut self.maps, first, Type::Bool)),
            "MutSetCursor" => {
                let cursor = Type::MutSetCursor(intern_map(&mut self.maps, first, Type::Bool));
                intern_option(
                    &mut self.option_types,
                    Type::Tuple(intern_tuple(&mut self.tuple_types, vec![first, cursor])),
                );
                cursor
            }
            _ => unreachable!("known type constructor"),
        };
        self.check_list_types(ty, span)?;
        Ok(ty)
    }
}
