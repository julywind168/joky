//! Dynamic routes are resolved without effects, then revalidated under the
//! executor-local registry's complete synchronous publication interval.
use super::super::access::Rejection;
use super::*;

pub(in super::super) struct Paths {
    descriptors: Vec<[u64; 6]>,
    alias: bool,
    module: LoadedModule,
    method: String,
    info: FunctionInfo,
    initial: Vec<DomainHandle>,
    branch: &'static str,
}
struct Selection {
    handles: Vec<DomainHandle>,
    branch: &'static str,
}
impl Paths {
    fn select(&self, domains: &Domains) -> Result<Selection, Rejection> {
        let mut handles: Vec<DomainHandle> = Vec::with_capacity(self.descriptors.len());
        for (i, descriptor) in self.descriptors.iter().enumerate() {
            let [kind, dependency, offset, directory, input, optional] = *descriptor;
            let reference = self.info.signature.params[i].reference_type().unwrap();
            let handle = if kind == 0 {
                let (sequence, _) = domains
                    .entries
                    .iter()
                    .find(|(_, d)| d.source_token == input / 2)
                    .ok_or(Rejection::Closed)?;
                Some(DomainHandle {
                    registry: domains.identity,
                    sequence: *sequence,
                    writable: input & 1 != 0,
                })
            } else {
                let key = if dependency == 0 {
                    input
                } else {
                    let handle = handles[(dependency - 1) as usize];
                    let domain = domains
                        .get(handle)
                        .map_err(|e| access::classify(domains, &[handle], &e))?;
                    // The descriptor's offset must identify a real UInt64 lane in
                    // the checked state schema, never arbitrary root memory.
                    if !domain
                        .ty
                        .storage_scalars()
                        .map_err(|_| Rejection::Incompatible)?
                        .contains(&(offset as usize, Type::UInt64))
                    {
                        return Err(Rejection::InvalidAccess);
                    }
                    // SAFETY: the root is live, the lane was checked above, and
                    // the registry borrow prevents publication during resolution.
                    unsafe {
                        (domain.root.as_ref().unwrap().scalar_pointer() as *const u8)
                            .add(offset as usize)
                            .cast::<u64>()
                            .read_unaligned()
                    }
                };
                directory::resolve_path(domains, directory, key)?
            };
            let Some(handle) = handle else {
                if optional != 0 {
                    return Ok(Selection {
                        handles,
                        branch: "missing",
                    });
                }
                return Err(Rejection::Missing(i as u64));
            };
            let domain = domains
                .get(handle)
                .map_err(|e| access::classify(domains, &[handle], &e))?;
            if !code::compatible(&domain.ty, &reference.target) {
                return Err(Rejection::Incompatible);
            }
            if reference.mutable && !handle.writable {
                return Err(Rejection::InvalidAccess);
            }
            for (j, previous) in handles.iter().enumerate() {
                if previous.sequence == handle.sequence
                    && (reference.mutable
                        || self.info.signature.params[j]
                            .reference_type()
                            .unwrap()
                            .mutable)
                {
                    if self.alias && i + 1 == self.descriptors.len() {
                        return Ok(Selection {
                            handles,
                            branch: "alias",
                        });
                    }
                    return Err(Rejection::Aliased((j * 64 + i) as u64));
                }
            }
            handles.push(handle);
        }
        Ok(Selection {
            handles,
            branch: "",
        })
    }
    fn entry(&self, branch: &str) -> Result<(String, FunctionInfo), Rejection> {
        let name = if branch.is_empty() {
            self.method.clone()
        } else {
            format!("{}/{branch}", self.method)
        };
        let info = self
            .module
            .instance_info(&name)
            .map_err(|_| Rejection::Incompatible)?;
        if !info.domain_entry || !crate::core::state_domain::value_eligible(&info) {
            return Err(Rejection::InvalidAccess);
        }
        Ok((name, info))
    }
}

pub(in super::super) fn submit(
    registry: &Rc<RefCell<Domains>>,
    module: LoadedModule,
    method: &str,
    descriptors: Vec<u64>,
    input: Vec<u64>,
    output: ValueOutput,
    limits: InterpreterLimits,
) -> Result<DomainRequest, Rejection> {
    let info = module
        .instance_info(method)
        .map_err(|_| Rejection::Incompatible)?;
    let count = crate::core::state_domain::access_count(&info);
    if !(1..=64).contains(&count)
        || descriptors.len() != 1 + count * 6
        || descriptors[0] > 1
        || !info.domain_entry
        || !method.starts_with("$when/")
        || !crate::core::state_domain::value_eligible(&info)
    {
        return Err(Rejection::InvalidAccess);
    }
    let mut paths = Paths {
        descriptors: descriptors[1..].as_chunks::<6>().0.to_vec(),
        alias: descriptors[0] != 0,
        module,
        method: method.to_owned(),
        info,
        initial: vec![],
        branch: "",
    };
    for (i, d) in paths.descriptors.iter().enumerate() {
        if d[0] > 1 || d[1] > i as u64 || d[5] > 1 || (d[5] != 0 && (i == 0 || i + 1 != count)) {
            return Err(Rejection::InvalidAccess);
        }
    }
    let domains = registry.borrow();
    let selection = paths.select(&domains)?;
    let (method, info) = paths.entry(selection.branch)?;
    if crate::core::state_domain::access_count(&info) != selection.handles.len() {
        return Err(Rejection::InvalidAccess);
    }
    let module = resolve(&domains, &selection.handles, &method, &info)
        .map_err(|e| access::classify(&domains, &selection.handles, &e))?;
    let anonymous_module = paths
        .method
        .starts_with("$when/anon/")
        .then(|| code::module_key(&paths.module));
    paths.initial = selection.handles.clone();
    paths.branch = selection.branch;
    let mut words = vec![0; selection.handles.len()];
    words.extend(input);
    let extra = words
        .len()
        .checked_add(count)
        .and_then(|n| n.checked_mul(8))
        .and_then(|n| {
            n.checked_add(
                std::mem::size_of::<Paths>()
                    + descriptors.len() * 8
                    + count * 2 * std::mem::size_of::<DomainHandle>()
                    + method.len()
                    + paths.method.len()
                    + std::mem::size_of::<RefCell<Vec<u64>>>()
                    + 2 * std::mem::size_of::<usize>()
                    + output.words.borrow().len() * 8,
            )
        })
        .ok_or(Rejection::ResourceLimit)?;
    drop(domains);
    enqueue(
        registry,
        0,
        Work::Pair(Pair {
            handles: selection.handles,
            paths: Some(Box::new(paths)),
            anonymous_module,
            module,
            method,
            info,
            words,
            limits,
            output: Some(output),
        }),
        extra,
    )
    .map_err(|e| Rejection::from_error(&e))
}

pub(in super::super) fn refresh(domains: &Domains, pair: &mut Pair) -> Result<(), Rejection> {
    let paths = pair.paths.as_ref().unwrap();
    let selection = paths.select(domains)?;
    if (selection.handles != paths.initial || selection.branch != paths.branch)
        && domains.limits.max_path_retries == 0
    {
        return Err(Rejection::RetryLimit);
    }
    // Local execution holds the complete registry interval. One recomputation
    // suffices: no directory or state can change before the body is invoked.
    let (method, info) = paths.entry(selection.branch)?;
    if crate::core::state_domain::access_count(&info) != selection.handles.len() {
        return Err(Rejection::InvalidAccess);
    }
    let input = pair.words.split_off(pair.handles.len());
    pair.words.clear();
    pair.words.resize(selection.handles.len(), 0);
    pair.words.extend(input);
    pair.handles = selection.handles;
    pair.method = method;
    pair.info = info;
    Ok(())
}
