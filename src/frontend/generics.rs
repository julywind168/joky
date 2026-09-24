use super::*;
use crate::syntax::{
    CallArgument, Expr, ExprKind, Function, NodeId, Parameter, TypeAnnotation, Visibility,
};
use crate::Span;

impl Frontend {
    pub(super) fn compile_generic_instances(
        &mut self,
        units: &[ModuleSourceUnit],
        graph: &ModuleGraph,
        metadata_map: &HashMap<StableId, ModuleMetadata>,
        cache: &ModuleCache,
        artifacts: &mut Vec<ModuleArtifact>,
        snapshots: &mut ModuleTypeSnapshots,
    ) -> Result<(), Diagnostic> {
        let mut cursor = 0;
        while cursor < artifacts.len() {
            let caller_id = artifacts[cursor].metadata.stable_id;
            let caller_hash = artifacts[cursor].metadata.abi_hash;
            // Own only the work list so artifacts can grow during instantiation;
            // cloning the caller's entire TypeTable also copied every AST/layout.
            let caller_types = &artifacts[cursor].interface;
            let mut requests = caller_types
                .generic_requests
                .iter()
                .map(|(key, request)| {
                    (
                        key.clone(),
                        request.clone(),
                        caller_types.generic_symbols[key].clone(),
                    )
                })
                .collect::<Vec<_>>();
            requests.sort_by(|a, b| a.0.cmp(&b.0));
            for (_, request, symbol) in requests {
                if artifacts
                    .iter()
                    .any(|a| a.metadata.stable_id == symbol.module)
                {
                    continue;
                }
                if artifacts.len() > units.len() + 1024 {
                    return Err(Diagnostic::codegen("generic instantiation limit exceeded"));
                }
                let defining = artifacts
                    .iter()
                    .find(|a| a.metadata.stable_id == request.definition.module)
                    .ok_or_else(|| Diagnostic::codegen("generic defining module is missing"))?;
                let original = units
                    .iter()
                    .find(|u| u.metadata.stable_id == request.definition.module)
                    .ok_or_else(|| Diagnostic::codegen("generic source unit is missing"))?;
                let mut unit = original.clone();
                unit.metadata.stable_id = symbol.module;
                unit.metadata.dependencies = defining.metadata.dependencies.clone();
                unit.metadata
                    .dependencies
                    .push((request.definition.module, defining.metadata.abi_hash));
                if caller_id != request.definition.module {
                    unit.metadata.dependencies.push((caller_id, caller_hash));
                }
                unit.metadata.dependencies.sort_unstable();
                unit.metadata.dependencies.dedup();
                unit.metadata.exports = vec![(symbol.name.clone(), Visibility::Public)];
                // Resolved parameter types are stored in public_abis; the textual
                // marker only distinguishes callable exports from constants.
                unit.metadata.signatures = vec![(symbol.name.clone(), "fn()->Unit!{}".into())];
                unit.metadata.abi_hash = crate::module::source_fingerprint(
                    format!("{:?}", unit.metadata.dependencies).as_bytes(),
                );
                unit.cache_key.module_identity = symbol.module;
                unit.cache_key.dependency_abi_hashes = unit
                    .metadata
                    .dependencies
                    .iter()
                    // Distinct entry modules can have identical public ABIs.
                    // The cached wrapper still records its caller's identity,
                    // so hash both fields rather than reusing another caller's
                    // dependency list from a shared cache directory.
                    .map(|(id, hash)| {
                        crate::module::source_fingerprint(
                            format!("{:016x}:{hash:016x}", id.0).as_bytes(),
                        )
                    })
                    .collect();
                unit.cache_key.dependency_abi_hashes.sort_unstable();
                if let Some(artifact) = cache
                    .load_artifact(&unit.cache_key)
                    .map_err(Diagnostic::codegen)?
                {
                    self.module_cache_hits += 1;
                    self.module_events.push(format!(
                        "hit generic {} [{:016x}]",
                        request.definition.name, symbol.module.0
                    ));
                    artifacts.push(artifact);
                    continue;
                }
                let mut program = defining
                    .template_program
                    .clone()
                    .ok_or_else(|| Diagnostic::codegen("generic template program is missing"))?;
                let definition = program
                    .functions
                    .iter()
                    .find(|f| f.name == request.definition.name)
                    .cloned()
                    .ok_or_else(|| Diagnostic::codegen("generic definition is missing"))?;
                let mut context = graph.compile_context(original.id, metadata_map);
                context.definition_module = Some(request.definition.module);
                snapshots.populate(&mut context, artifacts);
                let mut nodes = WrapperNodes(original.source.len() + 1);
                self.module_events.push(format!(
                    "compile generic {} [{:016x}]: {}",
                    request.definition.name,
                    symbol.module.0,
                    cache.miss_reason(&unit.cache_key)
                ));
                let mut arguments = Vec::new();
                for (index, ty) in request.arguments.iter().enumerate() {
                    let name = format!("@type{index}");
                    context.type_bindings.insert(name.clone(), (caller_id, *ty));
                    arguments.push(CallArgument {
                        label: None,
                        value: nodes.name(name),
                    });
                }
                let mut parameters = Vec::new();
                for (index, (label, ty)) in request.abi.parameters.iter().enumerate() {
                    let binding = format!("@parameter{index}");
                    context
                        .type_bindings
                        .insert(binding.clone(), (caller_id, *ty));
                    let span = nodes.span();
                    parameters.push(Parameter {
                        name: label.clone(),
                        borrowed: request.abi.parameter_borrows[index],
                        ty: TypeAnnotation::named(&binding, span),
                        span,
                    });
                    arguments.push(CallArgument {
                        label: Some(label.clone()),
                        value: nodes.name(label.clone()),
                    });
                }
                context
                    .type_bindings
                    .insert("@result".into(), (caller_id, request.abi.return_type));
                let callee = Box::new(nodes.name(definition.name));
                let body = nodes.expr(ExprKind::Call { callee, arguments });
                let span = nodes.span();
                program.functions.push(Function {
                    name: symbol.name.clone(),
                    foreign: None,
                    receiver_mode: None,
                    visibility: Visibility::Public,
                    type_parameters: vec![],
                    parameters,
                    return_type: Some(TypeAnnotation::named("@result", span)),
                    effect_names: definition.effect_names,
                    where_predicates: vec![],
                    body,
                    span,
                });
                let artifact = self
                    .compile_module_program(&unit, &context, program)
                    .inspect_err(|_| {
                        self.diagnostic_source = Some((
                            graph.module(original.id).path.clone(),
                            original.source.clone(),
                        ));
                    })?;
                self.module_compilations += 1;
                cache
                    .store_artifact(&unit.cache_key, &artifact)
                    .map_err(Diagnostic::codegen)?;
                cache
                    .store(&unit.cache_key, &artifact.metadata)
                    .map_err(Diagnostic::codegen)?;
                artifacts.push(artifact);
            }
            cursor += 1;
        }
        Ok(())
    }
}

// Synthetic wrappers live after the source's final byte. Every annotation and
// expression has a distinct identity; original template spans stay untouched.
struct WrapperNodes(usize);
impl WrapperNodes {
    fn span(&mut self) -> Span {
        self.0 += 1;
        Span::new(self.0, self.0 + 1)
    }
    fn expr(&mut self, kind: ExprKind) -> Expr {
        let span = self.span();
        Expr {
            id: NodeId::new(self.0 as u32),
            span,
            kind,
        }
    }
    fn name(&mut self, name: String) -> Expr {
        self.expr(ExprKind::Name(name))
    }
}
