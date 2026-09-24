//! Cross-module import resolution for independent compilation.

use crate::diagnostic::SemanticError;
use crate::module::{abi::AbiFunction, ModuleCompileContext, SymbolId};
use crate::syntax::{CallArgument, Expr, ExprKind, FieldAccess};
use crate::Span;

use super::checker::Checker;
use super::effects::EffectSet;
use super::expectation::TypeExpectation;
use super::symbol_table::FunctionSignature;

impl Checker {
    pub(super) fn configure_module_context(&mut self, context: &ModuleCompileContext) {
        self.definition_module = context.definition_module;
        self.type_query_depth = context.type_query_depth;
        self.standard_modules = context.standard_modules.clone();
        for module in &context.standard_modules {
            if let Some(table) = context.dependency_types.get(module) {
                for intrinsic in &table.interface.intrinsic_types {
                    self.register_intrinsic_type(intrinsic);
                }
            }
        }
        self.import_aliases = context.imports.clone();
        self.dependency_types = context.dependency_types.clone();
        self.dependency_exports = context.dependency_exports.clone();
    }

    pub(super) fn check_import_call(
        &mut self,
        callee: &Expr,
        arguments: &[CallArgument],
        call_span: Span,
        call_id: crate::syntax::NodeId,
        expectation: TypeExpectation,
    ) -> Result<Option<crate::sema::Type>, SemanticError> {
        let ExprKind::Field {
            value,
            access: FieldAccess::Name(member),
        } = &callee.kind
        else {
            return Ok(None);
        };
        let ExprKind::Name(alias) = &value.kind else {
            return Ok(None);
        };
        if !self.import_aliases.contains_key(alias) {
            return Ok(None);
        }
        // Public helpers can share a name with a raw effect operation. Calls
        // through the module use the helper; handler patterns still resolve
        // the effect operation, and the defining module can call it directly.
        let has_helper = self
            .dependency_exports
            .get(&self.import_aliases[alias])
            .is_some_and(|exports| exports.contains_key(member));
        if !has_helper
            && self
                .effects
                .by_name(alias)
                .is_some_and(|effect| self.effects.operation_by_name(effect, member).is_some())
        {
            return Ok(None);
        }
        let module = self.import_aliases[alias];
        let template = self
            .dependency_types
            .get(&module)
            .and_then(|table| table.interface.public_templates.get(member))
            .cloned();
        if let Some(template) = template {
            let definition = SymbolId {
                module,
                name: member.clone(),
            };
            let key = crate::module::symbol_key(&definition);
            if !self.functions.contains_key(&key) {
                let table = self.dependency_types[&module].clone();
                let parameters = template
                    .abi
                    .parameters
                    .iter()
                    .map(|(name, ty)| {
                        Ok((
                            name.clone(),
                            self.import_abi_type(module, &table, *ty, call_span)?,
                        ))
                    })
                    .collect::<Result<Vec<_>, SemanticError>>()?;
                let return_type =
                    self.import_abi_type(module, &table, template.abi.return_type, call_span)?;
                let mut declared_effects = super::EffectGroupSet::new();
                let mut used_effects = EffectSet::new();
                for name in &template.effects {
                    let group = self.import_abi_effect(module, name, call_span)?;
                    declared_effects.insert(group);
                    for op in self.effects.all_operations(group) {
                        used_effects.insert(op);
                    }
                }
                self.functions.insert(
                    key.clone(),
                    FunctionSignature {
                        receiver_mode: crate::syntax::ReceiverMode::Borrowed,
                        parameter_borrows: template.abi.parameter_borrows,
                        type_parameters: template.parameters,
                        type_parameter_bounds: template
                            .bounds
                            .into_iter()
                            .map(|bounds| {
                                bounds
                                    .into_iter()
                                    .map(|name| super::import_methods::trait_key(module, &name))
                                    .collect()
                            })
                            .collect(),
                        associated_bounds: Vec::new(),
                        parameters,
                        return_type,
                        declared_effects,
                        used_effects,
                    },
                );
                self.imported_templates.insert(key.clone(), definition);
            }
            let callee = Expr {
                id: callee.id,
                span: callee.span,
                kind: ExprKind::Name(key),
            };
            return self
                .check_call(&callee, arguments, call_span, call_id, expectation)
                .map(Some);
        }
        let symbol = self.resolve_import_field(alias, member, callee.span)?;
        let signature = self
            .functions
            .get(&crate::module::symbol_key(&symbol))
            .cloned()
            .expect("external function signature was registered");
        if arguments.len() != signature.parameters.len() {
            return Err(SemanticError::WrongArgumentCount {
                function: format!("{alias}.{member}"),
                expected: signature.parameters.len(),
                span: call_span,
            });
        }
        for (argument, (_, expected_type)) in self
            .order_arguments(arguments, &signature.parameters, member, call_span)?
            .into_iter()
            .zip(&signature.parameters)
        {
            self.check_expression(argument, TypeExpectation::require(*expected_type))?;
        }
        self.current_effects.extend(&signature.used_effects);
        self.external_symbols.insert(call_id, symbol);
        Ok(Some(signature.return_type))
    }

    pub(super) fn try_resolve_import_value(
        &mut self,
        expression: &Expr,
    ) -> Result<Option<crate::sema::Type>, SemanticError> {
        let ExprKind::Field {
            value,
            access: FieldAccess::Name(member),
        } = &expression.kind
        else {
            return Ok(None);
        };
        let ExprKind::Name(alias) = &value.kind else {
            return Ok(None);
        };
        if !self.import_aliases.contains_key(alias) {
            return Ok(None);
        }
        let module = self.import_aliases[alias];
        let table = self.dependency_types.get(&module).cloned();
        let Some((table, mut constant)) = table.and_then(|table| {
            table
                .interface
                .public_constants
                .get(member)
                .cloned()
                .map(|value| (table, value))
        }) else {
            return Err(SemanticError::UnknownValue {
                name: format!("{alias}.{member}"),
                span: expression.span,
            });
        };
        constant.map_types(&mut |ty| self.import_abi_type(module, &table, ty, expression.span))?;
        let ty = constant.ty;
        self.imported_constants.insert(expression.id, constant);
        Ok(Some(ty))
    }

    fn resolve_import_field(
        &mut self,
        alias: &str,
        member: &str,
        span: Span,
    ) -> Result<SymbolId, SemanticError> {
        let module =
            *self
                .import_aliases
                .get(alias)
                .ok_or_else(|| SemanticError::UnknownValue {
                    name: alias.to_owned(),
                    span,
                })?;
        let signature = self
            .dependency_exports
            .get(&module)
            .ok_or_else(|| SemanticError::UnknownFunction {
                name: member.to_owned(),
                span,
            })?
            .get(member)
            .cloned()
            .ok_or_else(|| SemanticError::UnknownFunction {
                name: member.to_owned(),
                span,
            })?;
        if signature.starts_with("fn(") {
            self.ensure_external_function(module, member, &signature, span)?;
        } else {
            return Err(SemanticError::UnknownValue {
                name: member.to_owned(),
                span,
            });
        }
        Ok(SymbolId {
            module,
            name: member.to_owned(),
        })
    }

    fn ensure_external_function(
        &mut self,
        module: crate::module::StableId,
        name: &str,
        signature: &str,
        span: Span,
    ) -> Result<(), SemanticError> {
        let key = crate::module::symbol_key(&SymbolId {
            module,
            name: name.to_owned(),
        });
        if self.functions.contains_key(&key) {
            return Ok(());
        }
        let abi =
            AbiFunction::parse(signature).map_err(|error| SemanticError::FunctionNotSupported {
                name: format!("invalid export signature for '{name}': {error}"),
                span,
            })?;
        let annotations = self.annotation_types.clone();
        let resolved = self
            .dependency_types
            .get(&module)
            .cloned()
            .and_then(|table| {
                table
                    .interface
                    .public_abis
                    .get(name)
                    .cloned()
                    .map(|abi| (table, abi))
            });
        let (parameters, return_type) = if let Some((table, abi)) = resolved {
            let parameters = abi
                .parameters
                .iter()
                .map(|(name, ty)| {
                    Ok((
                        name.clone(),
                        self.import_abi_type(module, &table, *ty, span)?,
                    ))
                })
                .collect::<Result<Vec<_>, SemanticError>>()?;
            (
                parameters,
                self.import_abi_type(module, &table, abi.return_type, span)?,
            )
        } else {
            let parameters = abi
                .parameters
                .iter()
                .map(|p| Ok((p.name.clone(), self.resolve_abi_type(module, &p.ty)?)))
                .collect::<Result<Vec<_>, SemanticError>>()?;
            (parameters, self.resolve_abi_type(module, &abi.result)?)
        };
        self.annotation_types = annotations;
        let parameter_borrows = abi
            .parameters
            .iter()
            .map(|p| p.borrowed)
            .collect::<Vec<_>>();
        let mut declared_effects = super::EffectGroupSet::new();
        for effect in &abi.effects {
            declared_effects.insert(self.import_abi_effect(module, effect, span)?);
        }
        let mut used_effects = EffectSet::new();
        let mut suspends = false;
        for group in declared_effects.iter() {
            for operation in self.effects.all_operations(group) {
                used_effects.insert(operation);
                suspends |= self
                    .effects
                    .operation_info(operation)
                    .is_some_and(|op| op.suspends);
            }
        }
        if let Some(actual) = self
            .dependency_types
            .get(&module)
            .and_then(|table| table.interface.pending_functions.get(name))
        {
            suspends = *actual;
        }
        self.external_signatures.insert(
            SymbolId {
                module,
                name: name.to_owned(),
            },
            super::type_table::ExternalFunction {
                region_contract: self
                    .dependency_types
                    .get(&module)
                    .and_then(|table| table.interface.public_abis.get(name))
                    .and_then(|abi| abi.region_contract.clone()),
                parameters: parameters.clone(),
                parameter_borrows: parameter_borrows.clone(),
                return_type,
                suspends,
            },
        );
        self.functions.insert(
            key,
            FunctionSignature {
                receiver_mode: crate::syntax::ReceiverMode::Borrowed,
                parameter_borrows,
                type_parameters: Vec::new(),
                type_parameter_bounds: Vec::new(),
                associated_bounds: Vec::new(),
                parameters,
                return_type,
                declared_effects,
                used_effects,
            },
        );
        Ok(())
    }
}
