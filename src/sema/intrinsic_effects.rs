//! Checked bindings from native handle methods to effect operations.

use super::checker::Checker;
use super::{EffectId, Type};
use crate::diagnostic::SemanticError;
use crate::module::ModuleCompileContext;
use crate::syntax::{IntrinsicType, Program, ReceiverMode, TypeAnnotation};

impl Checker {
    pub(super) fn bind_intrinsic_effects(
        &mut self,
        program: &Program,
        context: Option<&ModuleCompileContext>,
    ) -> Result<(), SemanticError> {
        if let Some(context) = context {
            let mut modules = context
                .standard_modules
                .iter()
                .chain(context.imports.values())
                .copied()
                .collect::<Vec<_>>();
            modules.sort();
            modules.dedup();
            for module in modules {
                let Some(table) = context.dependency_types.get(&module) else {
                    continue;
                };
                for intrinsic in &table.interface.intrinsic_types {
                    let Some(effect) = &intrinsic.effect else {
                        continue;
                    };
                    // Dependency declarations were checked in their defining
                    // environment. Remap the operation through its stable effect
                    // identity; never reinterpret its name in the caller.
                    let group = self.import_abi_effect(module, effect, intrinsic.span)?;
                    let receiver = self.intrinsic_effect_receiver(intrinsic)?;
                    self.register_intrinsic_effect_methods(intrinsic, receiver, group)?;
                }
            }
        }
        for intrinsic in &program.intrinsic_types {
            let Some(effect) = &intrinsic.effect else {
                continue;
            };
            let group = self.effects.by_name(effect).ok_or_else(|| {
                SemanticError::InvalidEffectDefinition {
                    name: format!("unknown intrinsic effect '{effect}'"),
                    span: intrinsic.span,
                }
            })?;
            let receiver = self.intrinsic_effect_receiver(intrinsic)?;
            for method in &intrinsic.methods {
                if !method.type_parameters.is_empty() {
                    return Err(SemanticError::FunctionNotSupported {
                        name: "generic intrinsic effect methods".into(),
                        span: method.span,
                    });
                }
                let operation = self
                    .effects
                    .operation_by_name(group, method.operation.as_deref().unwrap_or(&method.name))
                    .and_then(|id| self.effects.operation_info(id))
                    .cloned()
                    .ok_or_else(|| SemanticError::UnknownEffectOperation {
                        effect: effect.clone(),
                        operation: method.name.clone(),
                        span: method.span,
                    })?;
                let mut parameters = vec![receiver];
                for parameter in &method.parameters {
                    parameters.push(self.resolve_type(&parameter.ty)?);
                }
                let mut borrows = vec![method.receiver_mode == ReceiverMode::Borrowed];
                borrows.extend(method.parameters.iter().map(|parameter| parameter.borrowed));
                let result = self.resolve_type(&method.return_type)?;
                if parameters != operation.parameters
                    || borrows != operation.parameter_borrows
                    || result != operation.return_type
                {
                    return Err(SemanticError::FunctionNotSupported {
                        name: format!(
                            "intrinsic method '{}.{}' must match effect operation '{}.{}' (receiver, parameters, ownership and return type)",
                            intrinsic.name, method.name, effect, method.name
                        ),
                        span: method.span,
                    });
                }
            }
            self.register_intrinsic_effect_methods(intrinsic, receiver, group)?;
        }
        Ok(())
    }

    fn intrinsic_effect_receiver(
        &mut self,
        intrinsic: &IntrinsicType,
    ) -> Result<Type, SemanticError> {
        let receiver =
            self.resolve_type(&TypeAnnotation::named(&intrinsic.name, intrinsic.span))?;
        if !receiver.is_native_resource() || !intrinsic.type_parameters.is_empty() {
            return Err(SemanticError::FunctionNotSupported {
                name: "intrinsic effect bindings require a native resource receiver".into(),
                span: intrinsic.span,
            });
        }
        Ok(receiver)
    }

    fn register_intrinsic_effect_methods(
        &mut self,
        intrinsic: &IntrinsicType,
        receiver: Type,
        group: EffectId,
    ) -> Result<(), SemanticError> {
        let mut bindings = std::collections::HashMap::new();
        for method in &intrinsic.methods {
            let operation = self
                .effects
                .operation_by_name(group, method.operation.as_deref().unwrap_or(&method.name))
                .ok_or_else(|| SemanticError::UnknownEffectOperation {
                    effect: intrinsic.effect.clone().unwrap_or_default(),
                    operation: method.name.clone(),
                    span: method.span,
                })?;
            if bindings
                .insert((receiver, method.name.clone()), operation)
                .is_some()
            {
                return Err(SemanticError::FunctionNotSupported {
                    name: format!(
                        "duplicate intrinsic method '{}.{}'",
                        intrinsic.name, method.name
                    ),
                    span: method.span,
                });
            }
        }
        let existing = self
            .intrinsic_effect_methods
            .iter()
            .filter(|((ty, _), _)| *ty == receiver)
            .map(|(key, operation)| (key.clone(), *operation))
            .collect::<std::collections::HashMap<_, _>>();
        if !existing.is_empty() && existing != bindings {
            return Err(SemanticError::FunctionNotSupported {
                name: format!(
                    "conflicting intrinsic effect bindings for '{}'",
                    intrinsic.name
                ),
                span: intrinsic.span,
            });
        }
        self.intrinsic_effect_methods.extend(bindings);
        self.intrinsic_methods.insert(
            intrinsic.name.clone(),
            intrinsic
                .methods
                .iter()
                .cloned()
                .map(|m| (m.name.clone(), m))
                .collect(),
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::sema::check_program;
    use crate::syntax::parse_program;

    #[test]
    fn intrinsic_effect_binding_validates_unused_declarations() {
        let valid = "@intrinsic(effect = io) class File { fn peek(&self, count: UInt64) -> Bytes } eff io { fn peek(handle: &File, count: UInt64) -> Bytes } fn main() {}";
        check_program(&parse_program(valid).unwrap()).unwrap();
        for (from, to) in [
            ("effect = io", "effect = missing"),
            ("fn peek(&self", "fn missing(&self"),
            ("fn peek(&self", "fn peek(self"),
            ("handle: &File", "handle: &TcpStream"),
            (
                "count: UInt64) -> Bytes } eff",
                "count: Int32) -> Bytes } eff",
            ),
            (
                "count: UInt64) -> Bytes } eff",
                "count: UInt64) -> String } eff",
            ),
            (
                "count: UInt64) -> Bytes } eff",
                "count: UInt64, extra: Int32) -> Bytes } eff",
            ),
            (
                "fn peek(&self, count: UInt64) -> Bytes",
                "fn peek(&self, count: UInt64) -> Bytes; fn peek(&self, count: UInt64) -> Bytes",
            ),
        ] {
            let source = valid.replacen(from, to, 1);
            assert!(
                check_program(&parse_program(&source).unwrap()).is_err(),
                "accepted {source}"
            );
        }
        let borrowed = "@intrinsic(effect = io) class File { fn copy(&self, other: &File) -> Unit } eff io { fn copy(handle: &File, other: &File) -> Unit } fn main() {}";
        check_program(&parse_program(borrowed).unwrap()).unwrap();
        assert!(check_program(
            &parse_program(&borrowed.replacen("other: &File", "other: File", 1)).unwrap()
        )
        .is_err());
    }

    #[test]
    fn intrinsic_effect_methods_derive_effects_and_suspension_from_operations() {
        for suspends in [false, true] {
            let source = format!("@intrinsic(effect = io) class File {{ fn peek(&self) -> Int32 }} eff io {{ {} fn peek(handle: &File) -> Int32 }} fn inspect(handle: &File) -> Int32 effects {{ io }} {{ handle.peek() }} fn main() {{}}", if suspends { "@suspends" } else { "" });
            let table = check_program(&parse_program(&source).unwrap()).unwrap();
            let effects = table.function_effects("inspect").unwrap();
            let used = effects.iter().collect::<Vec<_>>();
            assert_eq!(used.len(), 1);
            assert_eq!(
                table.effects().operation_info(used[0]).unwrap().suspends,
                suspends
            );
            assert!(
                check_program(&parse_program(&source.replace(" effects { io }", "")).unwrap())
                    .is_err()
            );
        }
        let source = "@intrinsic(effect = file) class File { fn close(self) -> Unit } eff file { @suspends fn read_chunk(handle: &File, count: UInt64) -> Bytes; @suspends fn close(handle: File) -> Unit } fn inspect(handle: &File) -> Bytes effects { file } { handle.read_chunk(1) } fn main() {}";
        assert!(
            check_program(&parse_program(source).unwrap()).is_err(),
            "operations outside the declared method subset must not be exposed"
        );
    }
}
