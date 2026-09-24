use super::analysis::{
    collect_closure_functions, materialize_task_functions, GenericInstanceCollector,
};
use super::*;
use crate::syntax::visitor::ExprVisitor;
use crate::syntax::{Function, Program};

#[allow(dead_code)]
impl CoreProgram {
    pub(crate) fn lower(mut program: Program, mut types: CheckedTypes) -> Result<Self, Diagnostic> {
        program
            .functions
            .extend(std::mem::take(&mut types.prelude_functions));
        let source_spans = collect_source_spans(&program);
        let mut functions = Vec::new();
        for function in program.functions.iter().filter(|function| {
            function.type_parameters.is_empty()
                && !function
                    .return_type
                    .as_ref()
                    .is_some_and(|ty| ty.is_name("type"))
        }) {
            let id = CoreFunctionId(functions.len());
            functions.push(lower_function(
                function,
                id,
                module_id_for_function(&function.name),
                None,
                &types,
                &[],
                None,
            )?);
        }
        for class in &program.classes {
            let receiver = types.class_type(&class.name);
            for method in &class.methods {
                let id = CoreFunctionId(functions.len());
                functions.push(lower_function(
                    method,
                    id,
                    module_id_for_function(&method.name),
                    receiver,
                    &types,
                    &[],
                    None,
                )?);
            }
        }
        for structure in &program.structs {
            let receiver = types.struct_type(&structure.name);
            for method in &structure.methods {
                let id = CoreFunctionId(functions.len());
                functions.push(lower_function(
                    method,
                    id,
                    module_id_for_function(&method.name),
                    receiver,
                    &types,
                    &[],
                    None,
                )?);
            }
        }
        for implementation in &program.impls {
            let receiver = types
                .struct_type(&implementation.type_name)
                .or_else(|| types.class_type(&implementation.type_name))
                .ok_or_else(|| Diagnostic::codegen("trait impl target type was not resolved"))?;
            for method in &implementation.methods {
                let mut method = method.clone();
                let trait_name =
                    if let Some((alias, member)) = implementation.trait_name.split_once('.') {
                        types
                            .interface
                            .module_imports
                            .get(alias)
                            .map(|module| crate::sema::trait_key(*module, member))
                            .unwrap_or_else(|| implementation.trait_name.clone())
                    } else {
                        implementation.trait_name.clone()
                    };
                let is_static = types.is_static_trait_method(&trait_name, &method.name);
                method.name = crate::sema::trait_method_name(
                    types.interface.module_identity,
                    &trait_name,
                    &method.name,
                );
                let method = &method;
                let id = CoreFunctionId(functions.len());
                let mut lowered = lower_function(
                    method,
                    id,
                    module_id_for_function(&method.name),
                    Some(receiver),
                    &types,
                    &[],
                    None,
                )?;
                if is_static {
                    lowered.receiver = None;
                    lowered.receiver_mode = crate::syntax::ReceiverMode::Static;
                    lowered.name = types.static_method_name(receiver, &method.name);
                    lowered.visibility = Visibility::Public;
                }
                functions.push(lowered);
            }
        }

        let mut instances = types
            .generic_calls()
            .filter(|instance| !types.imported_templates.contains_key(&instance.function))
            .filter(|instance| {
                instance
                    .arguments
                    .iter()
                    .all(|argument| !types.contains_type_parameter(*argument))
            })
            .cloned()
            .map(|mut instance| {
                // Source argument erasure is call-site metadata, not part of
                // the identity of a monomorphized function.
                instance.compile_time_arguments = 0;
                instance
            })
            .collect::<Vec<_>>();
        let mut cursor = 0;
        while cursor < instances.len() {
            let instance = instances[cursor].clone();
            let definition = program
                .functions
                .iter()
                .find(|function| function.name == instance.function)
                .ok_or_else(|| Diagnostic::codegen("generic function definition was not found"))?;
            let mut collector = GenericInstanceCollector {
                types: &types,
                substitutions: &instance.arguments,
                instances: Vec::new(),
            };
            collector.visit_expr(&definition.body);
            for discovered in collector.instances {
                if !instances.contains(&discovered) {
                    instances.push(discovered);
                }
            }
            cursor += 1;
        }
        instances
            .sort_by_key(|instance| types.instance_name(&instance.function, &instance.arguments));
        instances.dedup();
        for instance in instances {
            let definition = program
                .functions
                .iter()
                .find(|function| function.name == instance.function)
                .ok_or_else(|| Diagnostic::codegen("generic function definition was not found"))?;
            let name = types.instance_name(&instance.function, &instance.arguments);
            let id = CoreFunctionId(functions.len());
            functions.push(lower_function(
                definition,
                id,
                module_id_for_function(&definition.name),
                None,
                &types,
                &instance.arguments,
                Some(name),
            )?);
        }

        if types.interface.module_identity.is_some() {
            let wrappers = functions
                .iter()
                .filter_map(|function| {
                    if function.name == crate::sema::DROP_METHOD {
                        return None;
                    }
                    let receiver = function.receiver?;
                    let nominal = match receiver {
                        Type::Class(id) => types.class_name(id),
                        Type::Struct(id) => types.struct_name(id),
                        _ => return None,
                    };
                    let mut wrapper = function.clone();
                    wrapper.body = CoreExpr {
                        id: function.body.id,
                        ty: function.return_type,
                        kind: CoreExprKind::Call {
                            callee: Box::new(CoreExpr {
                                id: function.body.id,
                                ty: Type::Unit,
                                kind: CoreExprKind::Field {
                                    value: Box::new(CoreExpr {
                                        id: function.body.id,
                                        ty: receiver,
                                        kind: CoreExprKind::Name("self".into()),
                                    }),
                                    access: FieldAccess::Name(function.name.clone()),
                                },
                            }),
                            type_arguments: vec![],
                            arguments: function
                                .parameters
                                .iter()
                                .map(|p| CoreCallArgument {
                                    label: Some(p.name.clone()),
                                    value: CoreExpr {
                                        id: function.body.id,
                                        ty: p.ty,
                                        kind: CoreExprKind::Name(p.name.clone()),
                                    },
                                })
                                .collect(),
                            effect_operation: None,
                        },
                    };
                    wrapper.name = format!("@method/{nominal}/{}", function.name);
                    wrapper.receiver = None;
                    wrapper.visibility = Visibility::Public;
                    wrapper.parameters.insert(
                        0,
                        CoreParameter {
                            name: "self".into(),
                            ty: receiver,
                        },
                    );
                    wrapper.parameter_ownership.insert(
                        0,
                        matches!(receiver, Type::Class(_)).then_some(
                            if function.receiver_mode == crate::syntax::ReceiverMode::Owned {
                                CoreParameterOwnership::Owned
                            } else {
                                CoreParameterOwnership::Borrowed
                            },
                        ),
                    );
                    Some(wrapper)
                })
                .collect::<Vec<_>>();
            for mut wrapper in wrappers {
                wrapper.id = CoreFunctionId(functions.len());
                functions.push(wrapper);
            }
        }

        // Materialize closure bodies as private functions. The closure
        // expression keeps the stable generated name; MIR can then represent
        // it as a first-class function value without an environment.
        let mut closure_functions = Vec::new();
        for function in &functions {
            collect_closure_functions(function, &mut closure_functions);
        }
        for (name, captures, capture_bindings, parameters, return_type, effects, body) in
            closure_functions
        {
            if functions.iter().any(|function| function.name == name) {
                continue;
            }
            let id = CoreFunctionId(functions.len());
            let closure_state = capture_bindings
                .iter()
                .any(|capture| capture.mutable)
                .then(|| CoreClosureState {
                    ty: types.closure_state_type(&captures),
                    captures: capture_bindings,
                });
            let captures = if let Some(state) = &closure_state {
                vec![("<closure-state>".to_owned(), state.ty)]
            } else {
                captures
            };
            let borrowed_parameters = captures.len();
            functions.push(CoreFunction {
                closure_state,
                foreign: None,
                id,
                module: module_id_for_function(&name),
                name,
                visibility: Visibility::Private,
                receiver: None,
                receiver_mode: crate::syntax::ReceiverMode::Borrowed,
                parameters: captures
                    .into_iter()
                    .chain(parameters)
                    .map(|(name, ty)| CoreParameter { name, ty })
                    .collect(),
                borrowed_parameters,
                parameter_ownership: Vec::new(),
                return_type,
                declared_effects: crate::sema::EffectGroupSet::default(),
                used_effects: effects.clone(),
                may_suspend: effects.may_suspend(types.effects()),
                body,
            });
        }

        for function in &functions {
            super::analysis::prepare_iteration_types(&function.body, &mut types);
        }
        materialize_task_functions(&mut functions, &types);

        let struct_defaults = types
            .struct_defaults()
            .iter()
            .map(|defaults| {
                defaults
                    .iter()
                    .map(|default| {
                        default
                            .as_ref()
                            // Symbolic type-function layouts remain templates;
                            // their defaults are checked after instantiation.
                            .filter(|default| types.get_optional(default).is_some())
                            .map(|default| super::lower_expr(default, &types, &[], &[]))
                            .transpose()
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .collect::<Result<Vec<_>, _>>()?;
        let class_defaults = program
            .classes
            .iter()
            .map(|class| {
                class
                    .fields
                    .iter()
                    .map(|field| {
                        field
                            .default
                            .as_ref()
                            .map(|default| super::lower_expr(default, &types, &[], &[]))
                            .transpose()
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .collect::<Result<Vec<_>, _>>()?;
        super::sorting::materialize_sort_functions(
            &mut functions,
            &mut types,
            struct_defaults
                .iter()
                .chain(&class_defaults)
                .flatten()
                .filter_map(Option::as_ref),
        );
        super::retain::materialize_retain_functions(
            &mut functions,
            &mut types,
            struct_defaults
                .iter()
                .chain(&class_defaults)
                .flatten()
                .filter_map(Option::as_ref),
        );
        super::dynamic::materialize_dynamic_methods(
            &mut functions,
            &types,
            struct_defaults
                .iter()
                .chain(&class_defaults)
                .flatten()
                .filter_map(Option::as_ref),
        );
        super::debug::materialize_default_debug(
            &mut functions,
            &types,
            struct_defaults
                .iter()
                .chain(&class_defaults)
                .flatten()
                .filter_map(Option::as_ref),
        );
        Ok(Self {
            source_spans,
            types,
            functions,
            struct_defaults,
            class_defaults,
        })
    }

    pub(crate) fn types(&self) -> &CheckedTypes {
        &self.types
    }

    pub(crate) fn functions(&self) -> &[CoreFunction] {
        &self.functions
    }

    pub(crate) fn struct_defaults(&self) -> &[Vec<Option<CoreExpr>>] {
        &self.struct_defaults
    }

    pub(crate) fn class_defaults(&self) -> &[Vec<Option<CoreExpr>>] {
        &self.class_defaults
    }
}

fn collect_source_spans(program: &Program) -> std::collections::HashMap<NodeId, Span> {
    #[derive(Default)]
    struct Spans(std::collections::HashMap<NodeId, Span>);
    impl ExprVisitor for Spans {
        type Output = ();
        fn visit_expr(&mut self, expr: &Expr) {
            self.0.insert(expr.id, expr.span);
            crate::syntax::visitor::walk_expr(self, expr);
        }
        fn default_output(&self) {}
    }
    let mut spans = Spans::default();
    crate::syntax::visitor::walk_program(&mut spans, program);
    for implementation in &program.impls {
        for method in &implementation.methods {
            spans.visit_expr(&method.body);
        }
    }
    for default in program
        .classes
        .iter()
        .flat_map(|item| &item.fields)
        .filter_map(|field| field.default.as_ref())
        .chain(
            program
                .structs
                .iter()
                .flat_map(|item| &item.fields)
                .filter_map(|field| field.default.as_ref()),
        )
    {
        spans.visit_expr(default);
    }
    spans.0
}

/// Imported functions are assigned a graph-derived `m<ID>_` symbol by the
/// package loader. Decode that internal symbol while the AST-to-HIR module
/// metadata transition is in progress.
pub(crate) fn module_id_for_function(name: &str) -> CoreModuleId {
    let Some(rest) = name.strip_prefix('m') else {
        return CoreModuleId(0);
    };
    let Some(separator) = rest.find('_') else {
        return CoreModuleId(0);
    };
    rest[..separator]
        .parse::<usize>()
        .map(CoreModuleId)
        .unwrap_or(CoreModuleId(0))
}

fn lower_function(
    function: &Function,
    id: CoreFunctionId,
    module: CoreModuleId,
    receiver: Option<Type>,
    types: &CheckedTypes,
    substitutions: &[Type],
    instance_name: Option<String>,
) -> Result<CoreFunction, Diagnostic> {
    let type_parameters = function
        .type_parameters
        .iter()
        .map(|parameter| parameter.name.clone())
        .collect::<Vec<_>>();
    let parameters = function
        .parameters
        .iter()
        .map(|parameter| {
            let ty = types
                .checked_annotation(&parameter.ty, substitutions, receiver)
                .ok_or_else(|| {
                    Diagnostic::codegen(format!("unresolved checked type '{}'", parameter.ty))
                })?;
            Ok(CoreParameter {
                name: parameter.name.clone(),
                ty,
            })
        })
        .collect::<Result<Vec<_>, Diagnostic>>()?;
    let return_type = function
        .return_type
        .as_ref()
        .map(|annotation| {
            types
                .checked_annotation(annotation, substitutions, receiver)
                .ok_or_else(|| {
                    Diagnostic::codegen(format!("unresolved checked type '{annotation}'"))
                })
        })
        .transpose()?
        .unwrap_or(Type::Unit);
    let qualified_name = receiver.map(|receiver| match receiver {
        Type::Struct(id) => format!("struct_{id}_{}", function.name),
        Type::Class(id) => format!("class_{id}_{}", function.name),
        _ => function.name.clone(),
    });
    let lookup_name = qualified_name.as_deref().unwrap_or(&function.name);
    let used_effects = types
        .function_effects(lookup_name)
        .or_else(|| types.function_effects(&function.name))
        .cloned()
        .unwrap_or_default();
    let declared_effects = types
        .function_declared_effects(lookup_name)
        .or_else(|| types.function_declared_effects(&function.name))
        .cloned()
        .unwrap_or_default();
    Ok(CoreFunction {
        closure_state: None,
        foreign: function.foreign.clone(),
        id,
        module,
        name: instance_name.unwrap_or_else(|| function.name.clone()),
        visibility: function.visibility,
        receiver,
        receiver_mode: function
            .receiver_mode
            .unwrap_or(crate::syntax::ReceiverMode::Borrowed),
        parameter_ownership: function
            .parameters
            .iter()
            .zip(&parameters)
            .map(|(source, parameter)| {
                (source.borrowed && types.is_owned(parameter.ty))
                    .then_some(CoreParameterOwnership::Borrowed)
            })
            .collect(),
        parameters,
        borrowed_parameters: 0,
        return_type,
        may_suspend: used_effects.may_suspend(types.effects()),
        declared_effects,
        used_effects,
        body: if function.foreign.is_some() {
            CoreExpr {
                id: function.body.id,
                ty: Type::Unit,
                kind: CoreExprKind::Unit,
            }
        } else {
            super::lower_expr(&function.body, types, substitutions, &type_parameters)?
        },
    })
}
