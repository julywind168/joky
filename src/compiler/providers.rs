//! Compiler-side installation of native provider operations.
//!
//! The compiler knows providers as a name plus the effects they serve;
//! operation names come from the checked `eff` declarations themselves. The
//! provider grouping is derived from the single runtime-effect registry in
//! `sema::effects::RUNTIME_EFFECTS`. The standard-module ↔ runtime-layout
//! contract is enforced by the tests in `contract.rs`, not by production
//! code: adding a provider is a registry row, a runtime dispatch row, and the
//! provider's own name-keyed hook table.

use crate::compiler::{ProviderDescriptor, ProviderOperationDescriptor};
use crate::sema::effects::RUNTIME_EFFECTS;
use crate::sema::TypeTable;
use joky_runtime::host::{ProviderOperationEntry, RuntimeScope};

#[cfg(test)]
pub(super) mod contract;
#[cfg(test)]
mod socket;

/// Provider registration groups derived from the runtime-effect registry:
/// provider name plus the effects it serves, in registry order. Entries for
/// effects a runtime build has no hooks for (non-unix `unix`/`unix_dgram`)
/// are skipped by name at registration, so no platform split is needed here.
fn provider_effects() -> Vec<(&'static str, Vec<&'static str>)> {
    let mut groups: Vec<(&'static str, Vec<&'static str>)> = Vec::new();
    for effect in RUNTIME_EFFECTS {
        let Some(provider) = effect.provider else {
            continue;
        };
        match groups.iter_mut().find(|(name, _)| *name == provider) {
            Some((_, effects)) => effects.push(effect.name),
            None => groups.push((provider, vec![effect.name])),
        }
    }
    groups
}

/// Derive `(effect, name, id)` registration entries for every provider whose
/// effects the checked program declares. Operation IDs are encoded as
/// `(effect_id << 32) | operation_index` and remain stable for the checked
/// program.
pub(super) fn provider_operations(types: &TypeTable) -> Vec<ProviderDescriptor> {
    let effects = types.effects();
    let mut providers = Vec::new();
    for (provider, effect_names) in provider_effects() {
        let mut operations = Vec::new();
        for effect_name in effect_names {
            let Some(effect) = effects.by_name(effect_name) else {
                continue;
            };
            for operation in effects.all_operations(effect) {
                let Some(info) = effects.operation_info(operation) else {
                    continue;
                };
                if !info.suspends {
                    // Only suspending operations dispatch to native hooks.
                    continue;
                }
                operations.push(ProviderOperationDescriptor {
                    effect: effect_name.to_owned(),
                    name: info.name.clone(),
                    operation: ((operation.effect.0 as u64) << 32) | operation.operation as u64,
                });
            }
        }
        if !operations.is_empty() {
            providers.push(ProviderDescriptor {
                name: provider.to_owned(),
                operations,
            });
        }
    }
    providers
}

/// Keep provider registrations alive until their runtime scope is closed.
pub(super) struct NativeProviderRegistry {
    _registrations: Vec<joky_runtime::host::ProviderRegistration>,
}

impl NativeProviderRegistry {
    pub(super) fn install(types: &TypeTable, scope: &RuntimeScope) -> Self {
        let _registrations = provider_operations(types)
            .into_iter()
            .filter_map(|provider| {
                let entries = provider
                    .operations
                    .iter()
                    .map(|operation| ProviderOperationEntry {
                        effect: &operation.effect,
                        name: &operation.name,
                        operation: operation.operation,
                    })
                    .collect::<Vec<_>>();
                scope.register_provider(&provider.name, &entries)
            })
            .collect();
        Self { _registrations }
    }
}
