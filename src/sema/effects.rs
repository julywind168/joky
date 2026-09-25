//! Effect type model
//!
//! Effects are computation-level interfaces: they describe the operations
//! code may request, not the methods a value supports. Parsing and handler
//! semantics hook into this module in later stages.

use std::collections::HashMap;

use super::types::Type;

/// Stable identifier for an effect group
#[derive(
    serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord,
)]
pub(crate) struct EffectId(pub(crate) usize);

/// Stable identifier for an effect operation
#[derive(
    serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord,
)]
pub(crate) struct EffectOperationId {
    pub(crate) effect: EffectId,
    pub(crate) operation: usize,
}

/// Control flow pattern of an effect operation
#[allow(dead_code)]
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EffectMode {
    Normal,
    Resumable,
    Aborts,
}

/// An operation within an effect group
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub(crate) struct EffectOperation {
    pub(crate) id: EffectOperationId,
    pub(crate) name: String,
    pub(crate) parameters: Vec<Type>,
    pub(crate) parameter_borrows: Vec<bool>,
    pub(crate) return_type: Type,
    pub(crate) mode: EffectMode,
    pub(crate) suspends: bool,
}

/// An effect group and its operations
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub(crate) struct EffectInfo {
    pub(crate) id: EffectId,
    pub(crate) name: String,
    pub(crate) identity: String,
    pub(crate) operations: Vec<EffectOperation>,
}

/// An effect whose operations are provided natively by the runtime
pub(crate) struct RuntimeEffect {
    pub(crate) name: &'static str,
    /// Built-in provider serving this effect operation; `None` means no
    /// blocking provider is used (e.g. `time.sleep` is handled by the reactor)
    pub(crate) provider: Option<&'static str>,
}

/// Single registry of runtime effects: it drives both the `@runtime/{name}`
/// identity rewrite here and the provider registration grouping in the
/// compiler's `PROVIDER_EFFECTS`; the runtime's `host::dispatch_provider`
/// holds the matching provider-name dispatch table. Adding an effect library
/// = one row in this table + one row in the runtime dispatch table + the
/// library itself
pub(crate) const RUNTIME_EFFECTS: &[RuntimeEffect] = &[
    RuntimeEffect {
        name: "process",
        provider: Some("process"),
    },
    RuntimeEffect {
        name: "time",
        provider: None,
    },
    RuntimeEffect {
        name: "file",
        provider: Some("file"),
    },
    RuntimeEffect {
        name: "env",
        provider: Some("env"),
    },
    RuntimeEffect {
        name: "tcp",
        provider: Some("socket"),
    },
    RuntimeEffect {
        name: "udp",
        provider: Some("socket"),
    },
    RuntimeEffect {
        name: "unix",
        provider: Some("socket"),
    },
    RuntimeEffect {
        name: "unix_dgram",
        provider: Some("socket"),
    },
    RuntimeEffect {
        name: "sqlite",
        provider: Some("sqlite"),
    },
    RuntimeEffect {
        name: "random",
        provider: Some("random"),
    },
];

pub(crate) fn is_runtime_effect(name: &str) -> bool {
    RUNTIME_EFFECTS.iter().any(|effect| effect.name == name)
}

impl EffectInfo {
    pub(crate) fn identity_in_module(&self, module: Option<crate::module::StableId>) -> String {
        let Some(module) = module.filter(|_| !self.identity.starts_with('@')) else {
            return self.identity.clone();
        };
        if is_runtime_effect(&self.name) {
            format!("@runtime/{}", self.name)
        } else {
            format!("@{:016x}/{}", module.0, self.name)
        }
    }
}

/// Effect registry for the semantic analysis stage
#[allow(dead_code)]
#[derive(serde::Serialize, serde::Deserialize, Debug, Default, Clone)]
pub(crate) struct EffectRegistry {
    effects: Vec<EffectInfo>,
    by_name: HashMap<String, EffectId>,
}

impl EffectRegistry {
    pub(crate) fn alias(&mut self, name: String, id: EffectId) {
        self.by_name.entry(name).or_insert(id);
    }
    pub(crate) fn iter(&self) -> impl Iterator<Item = &EffectInfo> {
        self.effects.iter()
    }
    pub(crate) fn define(&mut self, name: String) -> EffectId {
        if let Some(id) = self.by_name.get(&name).copied() {
            return id;
        }
        let id = EffectId(self.effects.len());
        self.effects.push(EffectInfo {
            id,
            name: name.clone(),
            identity: name.clone(),
            operations: Vec::new(),
        });
        self.by_name.insert(name, id);
        id
    }

    pub(crate) fn operation(
        &mut self,
        effect: EffectId,
        name: String,
        parameters: Vec<Type>,
        return_type: Type,
        mode: EffectMode,
        suspends: bool,
    ) -> EffectOperationId {
        let operations = &mut self
            .effects
            .get_mut(effect.0)
            .expect("effect id must come from this registry")
            .operations;
        let id = EffectOperationId {
            effect,
            operation: operations.len(),
        };
        operations.push(EffectOperation {
            id,
            name,
            parameter_borrows: vec![false; parameters.len()],
            parameters,
            return_type,
            mode,
            suspends,
        });
        id
    }

    pub(crate) fn by_name(&self, name: &str) -> Option<EffectId> {
        self.by_name.get(name).copied()
    }

    pub(crate) fn set_identity(&mut self, id: EffectId, identity: String) {
        self.effects[id.0].identity = identity;
    }
    pub(crate) fn replace_operation(&mut self, operation: EffectOperation) {
        let id = operation.id;
        self.effects[id.effect.0].operations[id.operation] = operation;
    }
    pub(crate) fn qualify(&mut self, module: crate::module::StableId) {
        for effect in &mut self.effects {
            effect.identity = effect.identity_in_module(Some(module));
        }
    }

    pub(crate) fn effect(&self, id: EffectId) -> Option<&EffectInfo> {
        self.effects.get(id.0)
    }

    pub(crate) fn operation_info(&self, id: EffectOperationId) -> Option<&EffectOperation> {
        self.effect(id.effect)
            .and_then(|effect| effect.operations.get(id.operation))
    }

    pub(crate) fn set_parameter_borrows(&mut self, id: EffectOperationId, modes: Vec<bool>) {
        let operation = &mut self.effects[id.effect.0].operations[id.operation];
        assert_eq!(operation.parameters.len(), modes.len());
        operation.parameter_borrows = modes;
    }

    #[allow(dead_code)]
    pub(crate) fn all_operations(
        &self,
        id: EffectId,
    ) -> impl Iterator<Item = EffectOperationId> + '_ {
        self.effect(id)
            .into_iter()
            .flat_map(|effect| effect.operations.iter().map(|operation| operation.id))
    }

    pub(crate) fn operation_by_name(
        &self,
        effect: EffectId,
        name: &str,
    ) -> Option<EffectOperationId> {
        self.effect(effect).and_then(|info| {
            info.operations
                .iter()
                .find(|operation| operation.name == name)
                .map(|operation| operation.id)
        })
    }

    #[allow(dead_code)]
    pub(crate) fn operation_mode(&self, operation: EffectOperationId) -> Option<EffectMode> {
        self.operation_info(operation)
            .map(|operation| operation.mode)
    }

    pub(crate) fn group_set(&self, names: &[String]) -> Option<EffectGroupSet> {
        let mut set = EffectGroupSet::new();
        for name in names {
            set.insert(self.by_name(name)?);
        }
        Some(set)
    }
}

/// Set of effect groups a function signature is allowed to use
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct EffectGroupSet {
    groups: Vec<EffectId>,
}

impl EffectGroupSet {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn insert(&mut self, group: EffectId) {
        if !self.groups.contains(&group) {
            self.groups.push(group);
        }
    }

    #[allow(dead_code)]
    pub(crate) fn contains(&self, group: EffectId) -> bool {
        self.groups.contains(&group)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = EffectId> + '_ {
        self.groups.iter().copied()
    }

    #[allow(dead_code)]
    pub(crate) fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }
}

/// Set of effect operations actually used by a function or computation
///
/// The set is deduplicated by operation ID and keeps insertion order; the IDs
/// assigned by semantic analysis are themselves stable, so ABI output or
/// diagnostics can later be sorted by ID when needed
#[allow(dead_code)]
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct EffectSet {
    operations: Vec<EffectOperationId>,
}

impl EffectSet {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn insert(&mut self, operation: EffectOperationId) {
        if !self.operations.contains(&operation) {
            self.operations.push(operation);
        }
    }

    pub(crate) fn contains(&self, operation: EffectOperationId) -> bool {
        self.operations.contains(&operation)
    }

    #[allow(dead_code)]
    pub(crate) fn union(&self, other: &Self) -> Self {
        let mut result = self.clone();
        result.extend(other);
        result
    }

    pub(crate) fn extend(&mut self, other: &Self) {
        for operation in &other.operations {
            self.insert(*operation);
        }
    }

    pub(crate) fn without(&self, handled: &Self) -> Self {
        Self {
            operations: self
                .operations
                .iter()
                .copied()
                .filter(|operation| !handled.contains(*operation))
                .collect(),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn without_one(&self, handled: EffectOperationId) -> Self {
        Self {
            operations: self
                .operations
                .iter()
                .copied()
                .filter(|operation| *operation != handled)
                .collect(),
        }
    }

    pub(crate) fn may_suspend(&self, registry: &EffectRegistry) -> bool {
        self.operations.iter().any(|operation| {
            registry
                .operation_info(*operation)
                .is_some_and(|operation| operation.suspends)
        })
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = EffectOperationId> + '_ {
        self.operations.iter().copied()
    }

    #[allow(dead_code)]
    pub(crate) fn is_empty(&self) -> bool {
        self.operations.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_assigns_stable_effect_and_operation_ids() {
        let mut registry = EffectRegistry::default();
        let clock = registry.define("Clock".to_owned());
        let sleep = registry.operation(
            clock,
            "sleep".to_owned(),
            vec![Type::U64],
            Type::Unit,
            EffectMode::Normal,
            true,
        );
        let now = registry.operation(
            clock,
            "now".to_owned(),
            Vec::new(),
            Type::I64,
            EffectMode::Normal,
            false,
        );

        assert_eq!(clock, EffectId(0));
        assert_eq!(sleep.operation, 0);
        assert_eq!(now.operation, 1);
        assert_eq!(registry.operation_info(sleep).unwrap().name, "sleep");
    }

    #[test]
    fn effect_set_deduplicates_and_combines_operations() {
        let first = EffectOperationId {
            effect: EffectId(0),
            operation: 0,
        };
        let second = EffectOperationId {
            effect: EffectId(1),
            operation: 0,
        };
        let mut left = EffectSet::new();
        left.insert(first);
        left.insert(first);
        let mut right = EffectSet::new();
        right.insert(second);
        right.insert(first);

        let combined = left.union(&right);
        assert_eq!(combined.iter().collect::<Vec<_>>(), vec![first, second]);
        assert!(combined.contains(first));
        assert!(!combined.without(&left).contains(first));
    }

    #[test]
    fn effect_set_reports_suspending_operations() {
        let mut registry = EffectRegistry::default();
        let clock = registry.define("Clock".to_owned());
        let sleep = registry.operation(
            clock,
            "sleep".to_owned(),
            vec![Type::U64],
            Type::Unit,
            EffectMode::Normal,
            true,
        );
        let mut effects = EffectSet::new();
        assert!(!effects.may_suspend(&registry));
        effects.insert(sleep);
        assert!(effects.may_suspend(&registry));
    }
}
