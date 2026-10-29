//! Read-only explanations of the same packet and call-graph gates used at dispatch.
use super::*;
use crate::{parser::ast::ConcurrentKind, SourceMap, Span, TypeRegistry};
use std::sync::Arc;

/// Static eligibility, not an observation of a task's eventual worker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParallelDisposition {
    Eligible,
    RuntimeChecked,
    Local,
    Rejected,
}

#[derive(Clone, Debug)]
pub struct ParallelSite {
    pub span: Span,
    pub function: String,
    pub construct: String,
    pub disposition: ParallelDisposition,
    pub reason: String,
}

#[derive(Clone, Debug)]
pub struct ParallelReport {
    pub sources: Arc<SourceMap>,
    pub entry_error: Option<Error>,
    pub sites: Vec<ParallelSite>,
}

/// Check a resolved standalone module using the standard host bindings, without
/// generating or running code. Custom host registration is not inferred from externs.
/// Sites reachable through concrete code dependencies are included; runtime source
/// provenance and the dynamic isolate context remain execution-time conditions.
pub fn explain_parallel(
    module: &crate::parser::ast::Module,
    types: &TypeRegistry,
    entry: &str,
) -> Result<ParallelReport, Error> {
    let registry = crate::stdlib::module_registry();
    let prepared = crate::core::prepared::PreparedModule::new(module, types, registry.metadata())?;
    let imports = registry.import_layouts(prepared.ast(), &HashMap::new())?;
    let plan = prepared.plan(&imports)?;
    let hosts = plan
        .functions
        .keys()
        .filter(|name| {
            registry
                .binding(name)
                .and_then(|b| b.host())
                .as_ref()
                .and_then(crate::runtime::parallel::Host::from_binding)
                .is_some()
        })
        .cloned()
        .collect();
    let effects = analyse(&plan, &prepared, &hosts);
    let rejected = scoped::rejections(&plan);
    let mut pending = vec![entry.to_owned()];
    let mut seen = HashSet::new();
    let mut sites = Vec::new();
    while let Some(name) = pending.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        let Some(body) = plan.bodies.get(&name) else {
            continue;
        };
        for (index, op) in body.ops.iter().enumerate() {
            pending.extend(op.dependencies().cloned());
            let (construct, inputs) = match op {
                Op::Concurrent {
                    kind: ConcurrentKind::Isolate,
                    ..
                } => {
                    sites.push(ParallelSite {
                        span: body.op_spans[index], function: display_name(&name),
                        construct: "isolate".into(), disposition: ParallelDisposition::Local,
                        reason: "isolate body stays on its worker; certified borrowed child groups may dispatch remotely".into(),
                    });
                    continue;
                }
                Op::Concurrent { kind, branches, .. } => (
                    kind.keyword(),
                    branches
                        .iter()
                        .map(|b| {
                            let signature = match b.indirect.as_ref().map(|s| &s.ty) {
                                Some(Type::Function(f)) => &f.signature,
                                _ => &plan.functions[&b.name].signature,
                            };
                            (b.name.as_str(), signature, b.indirect.is_some())
                        })
                        .collect::<Vec<_>>(),
                ),
                Op::TaskBranch { entry, .. } => (
                    "branch",
                    vec![(entry.as_str(), &plan.functions[entry].signature, false)],
                ),
                _ => continue,
            };
            let (disposition, reason) = match effects.require(&name) {
                Err(error) => (ParallelDisposition::Rejected, error.message().into()),
                Ok(()) => classify(&inputs, &plan, &effects, &rejected, construct),
            };
            sites.push(ParallelSite {
                span: body.op_spans[index],
                function: display_name(&name),
                construct: construct.into(),
                disposition,
                reason,
            });
        }
    }
    let sources = Arc::new(prepared.ast().sources().clone());
    sites.sort_by_key(|site| {
        (
            sources
                .get(site.span.source_id())
                .map(|file| file.name().to_owned())
                .unwrap_or_default(),
            site.span.start(),
            site.function.clone(),
            site.construct.clone(),
            site.reason.clone(),
        )
    });
    sites.dedup_by(|a, b| {
        a.span == b.span
            && a.function == b.function
            && a.construct == b.construct
            && a.reason == b.reason
    });
    Ok(ParallelReport {
        sources,
        entry_error: effects
            .require_packet(entry)
            .and_then(|_| {
                if plan.functions[entry].suspends {
                    Ok(())
                } else {
                    Err(Error::new_with_code(
                        ErrorPhase::Abi,
                        ErrorCode::InvalidCall,
                        "Parallel entry must be an @suspends function",
                    ))
                }
            })
            .err(),
        sites,
    })
}

fn classify(
    inputs: &[(&str, &crate::FunctionSignature, bool)],
    plan: &ModulePlan,
    effects: &Effects,
    rejected: &HashMap<String, String>,
    construct: &str,
) -> (ParallelDisposition, String) {
    use crate::runtime::parallel::{packet::Shape as Owned, scoped::InputShape};
    let local = |reason| (ParallelDisposition::Local, reason);
    for (name, _, indirect) in inputs {
        if !indirect {
            if let Err(error) = effects.require(name) {
                return (ParallelDisposition::Rejected, error.message().into());
            }
        }
    }
    let borrowed = inputs
        .iter()
        .any(|(_, sig, _)| sig.params.iter().any(|ty| Owned::of(ty).is_err()));
    for (branch, (name, sig, _)) in inputs.iter().enumerate() {
        let target = if construct == "branch" {
            "capture".into()
        } else {
            format!("branch {} `{}` argument", branch + 1, display_name(name))
        };
        for (index, ty) in sig.params.iter().enumerate() {
            if InputShape::of(ty).is_err() {
                return local(format!(
                    "{target} #{} has unsupported cross-worker input type `{ty}`",
                    index + 1
                ));
            }
        }
        if Owned::of(&sig.ret).is_err() {
            return local(format!(
                "branch {} has unsupported result packet type `{}`",
                branch + 1,
                sig.ret
            ));
        }
    }
    if borrowed {
        for (branch, (name, _, indirect)) in inputs.iter().enumerate() {
            if *indirect {
                return local(format!("branch {} uses an indirect target; borrowed groups require direct language entries", branch + 1));
            }
            if let Some(reason) = rejected.get(*name) {
                return local(format!(
                    "branch {} `{}`: {reason}",
                    branch + 1,
                    display_name(name)
                ));
            }
            if !plan.bodies.contains_key(*name) {
                return local(format!(
                    "branch {} starts at a host ABI; borrowed groups require language entries",
                    branch + 1
                ));
            }
        }
        (ParallelDisposition::RuntimeChecked, "read-only input shapes and call graph qualify; runtime must verify current-task ownership or a received bounded loan for every borrowed input".into())
    } else {
        (ParallelDisposition::Eligible, "owned input and result packets qualify outside isolate; inside isolate this group remains local".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(source: &str, entry: &str) -> ParallelReport {
        let vm = crate::vm::VmEngine::new();
        let types = vm.type_registry();
        let ast = types
            .resolve_with_modules(&crate::parser::parse(source).unwrap(), &Default::default())
            .unwrap();
        explain_parallel(&ast, types, entry).unwrap_or_else(|error| panic!("{error}"))
    }

    #[test]
    fn parallel_report_traces_scoped_call_graph_and_concrete_dependencies() {
        let report = report(
            r#"
            import joky/array.{type Array}
            import joky/task
            fn number() -> Int32 { 1 }
            fn callback() -> Int32 { let f = number; f() }
            fn reader(values: &Array<Int32>) @suspends -> Int32 { callback() }
            fn printer(values: &Array<Int32>) @suspends -> Int32 { println(1); 1 }
            fn recurse(values: &Array<Int32>, n: Int32) @suspends -> Int32 {
                if n == 0 { 1 } else { recurse(values, n - 1) }
            }
            fn main() @suspends {
                let values = [1]
                join { | reader(&values) | recurse(&values, 1) };
                join { | printer(&values) | recurse(&values, 1) };
                isolate { join { | recurse(&values, 1) | recurse(&values, 2) }; };
            }
            fn unreachable() @suspends { let values = [1]; join { | recurse(&values, 1) | recurse(&values, 2) }; }
        "#,
            "main",
        );
        assert!(report.entry_error.is_none(), "{:?}", report.entry_error);
        assert_eq!(report.sites.len(), 4, "{:?}", report.sites);
        let local = report
            .sites
            .iter()
            .find(|s| s.construct == "join" && s.disposition == ParallelDisposition::Local)
            .unwrap();
        assert!(local.reason.contains("printer"), "{}", local.reason);
        assert_eq!(
            report
                .sites
                .iter()
                .filter(|s| s.construct == "join"
                    && s.disposition == ParallelDisposition::RuntimeChecked)
                .count(),
            2
        );
        assert!(report
            .sites
            .iter()
            .any(|s| s.disposition == ParallelDisposition::Local
                && s.reason.contains("host reference ABI")));
        assert!(
            report
                .sites
                .iter()
                .any(|s| s.construct == "join"
                    && s.disposition == ParallelDisposition::RuntimeChecked)
        );
        assert!(report
            .sites
            .iter()
            .any(|s| s.construct == "isolate" && s.disposition == ParallelDisposition::Local));
    }

    #[test]
    fn parallel_report_scoped_callbacks_include_unused_candidates_and_cleanup() {
        for body in [
            "println(value); value",
            r#"let guard = Guard { value: "cleanup" }; value"#,
        ] {
            let source = format!(
                r#"
                import joky/array.{{type Array}}
                import joky/drop.{{type Drop}}
                struct Guard {{ value: String }}
                impl Drop for Guard {{ fn drop(&mut self) {{ println(self.value) }} }}
                fn safe(value: Int32) -> Int32 {{ value }}
                fn bad(value: Int32) -> Int32 {{ {body} }}
                fn expose() -> fn(Int32) -> Int32 {{ bad }}
                fn reader(values: &Array<Int32>, callback: fn(Int32) -> Int32) @suspends -> Int32 {{ callback(values[0]) }}
                fn main() @suspends {{
                    let values = [1]
                    join {{ | reader(&values, safe) | reader(&values, safe) }};
                }}
            "#
            );
            let report = report(&source, "main");
            assert!(report.entry_error.is_none(), "{:?}", report.entry_error);
            assert_eq!(report.sites.len(), 1);
            let site = &report.sites[0];
            assert_eq!(site.disposition, ParallelDisposition::Local);
            for witness in ["reader", "bad", "host reference ABI"] {
                assert!(site.reason.contains(witness), "{}", site.reason);
            }
            if body.contains("Guard") {
                assert!(site.reason.contains("drop"), "{}", site.reason);
            }
        }
    }

    #[test]
    fn parallel_report_aggregate_borrows_use_shared_admission() {
        let report = report(
            r#"
            import joky/array.{type Array}
            struct Point { x: Int32, y: Float64 }
            fn read(values: &Array<Point>) @suspends {}
            fn main() @suspends {
                let values = [Point { x: 1, y: 2.5 }]
                let view = &values
                join { | read(view) | read(view) };
                scope { branch { read(view) } }
            }
        "#,
            "main",
        );
        assert!(report.entry_error.is_none());
        assert_eq!(report.sites.len(), 2);
        assert!(report
            .sites
            .iter()
            .all(|s| s.disposition == ParallelDisposition::RuntimeChecked));
    }

    #[test]
    fn parallel_report_distinguishes_entry_rejection_from_local_fallback() {
        let source = "extern fn unknown() fn helper() @suspends { unknown() } fn main() @suspends { join { | helper() | helper() }; }";
        let rejected = report(source, "main");
        assert!(rejected
            .entry_error
            .unwrap()
            .message()
            .contains("ParallelSafe"));
        assert_eq!(rejected.sites.len(), 1);
        assert_eq!(rejected.sites[0].disposition, ParallelDisposition::Rejected);
        assert!(rejected.sites[0].reason.contains("unknown"));
        let missing = report(source, "missing");
        assert!(missing.entry_error.is_some());
        assert!(missing.sites.is_empty());
    }
}
