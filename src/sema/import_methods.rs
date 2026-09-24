use super::checker::Checker;
use super::symbol_table::FunctionSignature;
use super::{EffectGroupSet, EffectSet, ModuleTypes, Type};
use crate::diagnostic::SemanticError;
use crate::module::{StableId, SymbolId};
use crate::Span;

pub(crate) fn trait_key(module: StableId, name: &str) -> String {
    if matches!(
        name,
        "Drop"
            | "Show"
            | "Debug"
            | "Eq"
            | "PartialEq"
            | "PartialOrd"
            | "Ord"
            | "Hash"
            | "FromString"
            | "Cursor"
            | "__MapKey"
    ) || name.starts_with('@')
    {
        name.into()
    } else {
        crate::module::symbol_key(&SymbolId {
            module,
            name: name.into(),
        })
    }
}

impl Checker {
    pub(super) fn import_nominal_methods(
        &mut self,
        module: StableId,
        table: &ModuleTypes,
        source: Type,
        receiver: Type,
        span: Span,
    ) -> Result<(), SemanticError> {
        for (name, values) in table.associated_impls(source) {
            let values = values
                .iter()
                .map(|(name, ty)| {
                    Ok((
                        name.clone(),
                        self.import_abi_type(module, table, *ty, span)?,
                    ))
                })
                .collect::<Result<_, SemanticError>>()?;
            self.trait_associated_impls
                .insert((trait_key(module, name), receiver), values);
        }
        for (name, implementations) in &table.trait_implementations {
            if implementations.contains(&source) {
                self.trait_impls
                    .entry(trait_key(module, name))
                    .or_default()
                    .insert(receiver);
                if name == "Show" {
                    self.show_impls.insert(receiver);
                }
            }
        }
        for method in table
            .interface
            .methods
            .iter()
            .filter(|m| m.receiver == source)
        {
            let mut abi = method.abi.clone();
            abi.parameters = abi
                .parameters
                .iter()
                .map(|(name, ty)| {
                    Ok((
                        name.clone(),
                        self.import_abi_type(module, table, *ty, span)?,
                    ))
                })
                .collect::<Result<_, SemanticError>>()?;
            abi.return_type = self.import_abi_type(module, table, abi.return_type, span)?;
            let mut declared_effects = EffectGroupSet::new();
            let mut used_effects = EffectSet::new();
            for name in &method.declared_effects {
                declared_effects.insert(self.import_abi_effect(
                    method.symbol.module,
                    name,
                    span,
                )?);
            }
            for name in &method.effects {
                let group = self.import_abi_effect(method.symbol.module, name, span)?;
                for op in self.effects.all_operations(group) {
                    used_effects.insert(op);
                }
            }
            let offset = usize::from(method.receiver_mode != crate::syntax::ReceiverMode::Static);
            let signature = FunctionSignature {
                receiver_mode: method.receiver_mode,
                parameter_borrows: abi.parameter_borrows[offset..].to_vec(),
                parameters: abi.parameters[offset..].to_vec(),
                return_type: abi.return_type,
                type_parameters: vec![],
                type_parameter_bounds: vec![],
                associated_bounds: vec![],
                declared_effects,
                used_effects,
            };
            match receiver {
                Type::Class(id) => {
                    self.classes
                        .values_mut()
                        .find(|v| v.id == id)
                        .unwrap()
                        .methods
                        .insert(method.name.clone(), signature);
                }
                Type::Struct(id) => {
                    self.structs
                        .values_mut()
                        .find(|v| v.id == id)
                        .unwrap()
                        .methods
                        .insert(method.name.clone(), signature);
                }
                _ => continue,
            }
            self.imported_method_abis
                .push(crate::module::generics::MethodAbi {
                    receiver,
                    receiver_mode: method.receiver_mode,
                    name: method.name.clone(),
                    symbol: method.symbol.clone(),
                    abi: abi.clone(),
                    effects: method.effects.clone(),
                    declared_effects: method.declared_effects.clone(),
                });
            self.external_signatures.insert(method.symbol.clone(), abi);
            self.method_symbols
                .insert((receiver, method.name.clone()), method.symbol.clone());
        }
        Ok(())
    }
}
