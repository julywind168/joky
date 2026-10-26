//! Dynamic routes are resolved without effects, then revalidated under the
//! executor-local registry's complete synchronous publication interval.
use super::super::access::Rejection;
use super::*;

pub(in super::super) struct Paths {
    registry: Weak<RefCell<Domains>>,
    source_key: usize,
    descriptors: Vec<[u64; 6]>,
    keys: Vec<String>,
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
    creations: Vec<(usize, u64, directory::Key)>,
}
impl Paths {
    pub(in super::super) fn needs_clock(&self, domains: &Domains) -> bool {
        let Ok(selection) = self.select(domains) else {
            return false;
        };
        let module = selection
            .handles
            .iter()
            .find(|h| h.sequence != 0)
            .and_then(|h| domains.get(*h).ok())
            .map(|d| d.module.clone())
            .or_else(|| code::creation_module(domains, &self.module).ok());
        let Some(module) = module else {
            return false;
        };
        selection.creations.iter().any(|(index, _, _)| {
            timers::automatic::has_declarations(
                &module,
                &self.info.signature.params[*index]
                    .reference_type()
                    .unwrap()
                    .target,
            )
        })
    }

    pub(in super::super) fn changed(&self, domains: &Domains) -> Result<bool, Rejection> {
        let selection = self.select(domains)?;
        Ok(selection.handles != self.initial || selection.branch != self.branch)
    }
    pub(in super::super) fn retry(&mut self, domains: &Domains) -> Result<(), Rejection> {
        let selection = self.select(domains)?;
        self.initial = selection.handles;
        self.branch = selection.branch;
        Ok(())
    }
    pub(in super::super) fn accesses(
        &self,
        domains: &Domains,
    ) -> Result<Vec<(u64, bool, bool)>, Rejection> {
        let selection = self.select(domains)?;
        let mut accesses = Vec::new();
        for (handle, ty) in selection.handles.iter().zip(&self.info.signature.params) {
            if handle.sequence != 0 {
                accesses.push((handle.sequence, false, ty.reference_type().unwrap().mutable));
            }
        }
        for descriptor in &self.descriptors {
            if descriptor[0] & 3 != 0 {
                accesses.push((descriptor[3], true, descriptor[0] & 4 != 0));
            }
        }
        Ok(accesses)
    }
    fn select(&self, domains: &Domains) -> Result<Selection, Rejection> {
        let mut handles: Vec<DomainHandle> = Vec::with_capacity(self.descriptors.len());
        let mut creations: Vec<(usize, u64, directory::Key)> = Vec::new();
        for (i, descriptor) in self.descriptors.iter().enumerate() {
            let [kind, dependency, offset, directory, input, optional] = *descriptor;
            let create = kind & 4 != 0;
            let kind = kind & 3;
            let mut pending = None;
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
                    if kind == 1 {
                        directory::Key::Index(input)
                    } else {
                        directory::Key::Name(
                            self.keys
                                .get(input as usize)
                                .ok_or(Rejection::InvalidAccess)?
                                .clone(),
                        )
                    }
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
                        .contains(&(
                            offset as usize,
                            if kind == 1 {
                                Type::UInt64
                            } else {
                                Type::String
                            },
                        ))
                    {
                        return Err(Rejection::InvalidAccess);
                    }
                    // SAFETY: the root is live, the lane was checked above, and
                    // the registry borrow prevents publication during resolution.
                    unsafe {
                        let pointer = (domain.root.as_ref().unwrap().scalar_pointer() as *const u8)
                            .add(offset as usize);
                        if kind == 1 {
                            directory::Key::Index(pointer.cast::<u64>().read_unaligned())
                        } else {
                            directory::Key::Name(
                                crate::runtime::owned_text::borrow(
                                    pointer
                                        .cast::<*const crate::runtime::buffer::Buffer>()
                                        .read_unaligned(),
                                )
                                .map_err(|_| Rejection::InvalidAccess)?
                                .to_owned(),
                            )
                        }
                    }
                };
                match directory::resolve_key(domains, directory, &key)? {
                    Some(handle) => Some(handle),
                    None if create => {
                        pending = Some((i, directory, key));
                        Some(DomainHandle {
                            registry: domains.identity,
                            sequence: 0,
                            writable: true,
                        })
                    }
                    None => None,
                }
            };
            let Some(handle) = handle else {
                if optional != 0 {
                    return Ok(Selection {
                        handles,
                        branch: "missing",
                        creations,
                    });
                }
                return Err(Rejection::Missing(i as u64));
            };
            if handle.sequence != 0 {
                let domain = domains
                    .get(handle)
                    .map_err(|e| access::classify(domains, &[handle], &e))?;
                if !code::compatible(&domain.ty, &reference.target) {
                    return Err(Rejection::Incompatible);
                }
                if reference.mutable && !handle.writable {
                    return Err(Rejection::InvalidAccess);
                }
            }
            for (j, previous) in handles.iter().enumerate() {
                if ((handle.sequence != 0 && previous.sequence == handle.sequence)
                    || pending.as_ref().is_some_and(|(_, dir, key)| {
                        creations.iter().any(|(index, previous_dir, previous_key)| {
                            *index == j && dir == previous_dir && key == previous_key
                        })
                    }))
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
                            creations,
                        });
                    }
                    return Err(Rejection::Aliased((j * 64 + i) as u64));
                }
            }
            handles.push(handle);
            if let Some(pending) = pending {
                creations.push(pending);
            }
        }
        Ok(Selection {
            handles,
            branch: "",
            creations,
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
    packet: Packet,
    input: Vec<u64>,
    output: ValueOutput,
    limits: InterpreterLimits,
) -> Result<DomainRequest, Rejection> {
    let Packet {
        descriptors,
        keys,
        source_key,
    } = packet;
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
        registry: Rc::downgrade(registry),
        source_key,
        descriptors: descriptors[1..].as_chunks::<6>().0.to_vec(),
        keys,
        alias: descriptors[0] != 0,
        module,
        method: method.to_owned(),
        info,
        initial: vec![],
        branch: "",
    };
    for (i, d) in paths.descriptors.iter().enumerate() {
        if !matches!(d[0], 0 | 1 | 2 | 5 | 6)
            || d[1] > i as u64
            || d[5] > 1
            || (d[5] != 0 && (i == 0 || i + 1 != count))
        {
            return Err(Rejection::InvalidAccess);
        }
    }
    let domains = registry.borrow();
    let selection = paths.select(&domains)?;
    let (method, info) = paths.entry(selection.branch)?;
    if crate::core::state_domain::access_count(&info) != selection.handles.len() {
        return Err(Rejection::InvalidAccess);
    }
    let module = if let Some(handle) = selection.handles.iter().find(|h| h.sequence != 0) {
        domains
            .get(*handle)
            .map_err(|e| access::classify(&domains, &[*handle], &e))?
            .module
            .clone()
    } else {
        code::creation_module(&domains, &paths.module).map_err(|e| Rejection::from_error(&e))?
    };
    let current = module
        .instance_info(&method)
        .map_err(|_| Rejection::Incompatible)?;
    if !code::compatible_entry(&info, &current) {
        return Err(Rejection::Incompatible);
    }
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
                    + paths
                        .keys
                        .iter()
                        .map(|key| key.len() + std::mem::size_of::<String>())
                        .sum::<usize>()
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

pub(in super::super) struct Staged {
    registry: Weak<RefCell<Domains>>,
    roots: Vec<u64>,
    publications: Vec<directory::Publication>,
    _charge: reads::Charge,
}
impl Staged {
    pub(in super::super) fn prepare_automatic(
        &self,
        domains: &mut Domains,
        now: Option<Instant>,
    ) -> Result<(), Error> {
        for sequence in &self.roots {
            timers::automatic::prepare_new(
                domains,
                &self.registry,
                DomainHandle {
                    registry: domains.identity,
                    sequence: *sequence,
                    writable: true,
                },
                now,
            )?;
        }
        Ok(())
    }
    pub(in super::super) fn validate(&self, domains: &Domains) -> Result<(), Rejection> {
        for publication in &self.publications {
            publication.validate(domains)?;
        }
        Ok(())
    }
    pub(in super::super) fn publish(&mut self, domains: &mut Domains) {
        for publication in self.publications.drain(..) {
            publication.publish(domains);
        }
        for sequence in &self.roots {
            timers::automatic::activate_new(&domains.entries[sequence]);
        }
        self.roots.clear();
    }
    pub(in super::super) fn discard(self, domains: &mut Domains) -> Vec<TaskId> {
        let mut jobs = Vec::new();
        for id in self.roots {
            if let Some(domain) = domains.entries.remove(&id) {
                jobs.extend(domain.tasks.keys().copied());
            }
        }
        jobs
    }
}
pub(in super::super) enum Failure {
    Rejected(Rejection),
    Constructor(Error),
}
impl From<Rejection> for Failure {
    fn from(reason: Rejection) -> Self {
        Self::Rejected(reason)
    }
}
pub(in super::super) fn refresh(domains: &mut Domains, pair: &mut Pair) -> Result<Staged, Failure> {
    refresh_with(
        domains,
        pair,
        |_, module, info, constructor, input, limits, budget| {
            Root::construct(module, info, constructor, input, limits, budget).map(Some)
        },
    )
}
pub(in super::super) fn refresh_with(
    domains: &mut Domains,
    pair: &mut Pair,
    mut construct: impl FnMut(
        u64,
        &LoadedModule,
        &FunctionInfo,
        &str,
        &[u64],
        InterpreterLimits,
        std::sync::Arc<Budget>,
    ) -> Result<Option<Root>, Error>,
) -> Result<Staged, Failure> {
    let paths = pair.paths.as_ref().unwrap();
    let mut selection = paths.select(domains)?;
    if (selection.handles != paths.initial || selection.branch != paths.branch)
        && domains.limits.max_path_retries == 0
    {
        return Err(Rejection::RetryLimit.into());
    }
    let (method, info) = paths.entry(selection.branch)?;
    if crate::core::state_domain::access_count(&info) != selection.handles.len() {
        return Err(Rejection::InvalidAccess.into());
    }
    let (module, source_module) =
        if let Some(handle) = selection.handles.iter().find(|h| h.sequence != 0) {
            let domain = domains
                .get(*handle)
                .map_err(|e| access::classify(domains, &[*handle], &e))?;
            (domain.module.clone(), domain.source_module)
        } else {
            let module = code::creation_module(domains, &paths.module)
                .map_err(|e| Rejection::from_error(&e))?;
            let key = if source::same_module(&module, &paths.module) {
                paths.source_key
            } else {
                code::module_key(&module)
            };
            (module, key)
        };
    let current = module
        .instance_info(&method)
        .map_err(|_| Rejection::Incompatible)?;
    if !code::compatible_entry(&info, &current) {
        return Err(Rejection::Incompatible.into());
    }
    if pair
        .anonymous_module
        .is_some_and(|key| key != code::module_key(&module))
    {
        return Err(Rejection::Incompatible.into());
    }
    let count = selection.creations.len();
    let charge = reads::Charge::new(
        domains.budget.clone(),
        count * (std::mem::size_of::<u64>() + std::mem::size_of::<directory::Publication>()),
    )
    .map_err(|_| Rejection::ResourceLimit)?;
    let input = pair.words.split_off(pair.handles.len());
    let mut staged = Staged {
        registry: paths.registry.clone(),
        roots: Vec::with_capacity(count),
        publications: Vec::with_capacity(count),
        _charge: charge,
    };
    let result = (|| {
        for (index, directory, key) in selection.creations {
            if let Some(handle) = staged
                .publications
                .iter()
                .find_map(|p| p.target_for(directory, &key))
            {
                selection.handles[index] = handle;
                continue;
            }
            let constructor = format!("{}/create/{index}", paths.method);
            let before = paths
                .module
                .instance_info(&constructor)
                .map_err(|_| Rejection::Incompatible)?;
            let info = module
                .instance_info(&constructor)
                .map_err(|_| Rejection::Incompatible)?;
            if !info.domain_entry
                || !crate::core::state_domain::constructor_eligible(&info)
                || !code::compatible_entry(&before, &info)
            {
                return Err(Rejection::Incompatible.into());
            }
            let reference = paths.info.signature.params[index].reference_type().unwrap();
            if !code::compatible(&reference.target, &info.signature.ret) {
                return Err(Rejection::Incompatible.into());
            }
            // Admission reserves directory space and key ownership before running a constructor.
            let pending = staged
                .publications
                .iter()
                .filter(|p| p.directory == directory)
                .count();
            let mut publication =
                directory::prepare_creation(domains, &paths.registry, directory, key, pending)?;
            if domains.entries.len() >= domains.limits.max_domains {
                return Err(Rejection::ResourceLimit.into());
            }
            let sequence = domains.next_domain;
            let next = sequence.checked_add(1).ok_or(Rejection::ResourceLimit)?;
            domains
                .entries
                .try_reserve(1)
                .map_err(|_| Rejection::ResourceLimit)?;
            let token = source::next_token().map_err(|_| Rejection::ResourceLimit)?;
            let schema = source::next_token().map_err(|_| Rejection::ResourceLimit)?;
            let root = construct(
                sequence,
                &module,
                &info,
                &constructor,
                &input,
                pair.limits,
                domains.budget.clone(),
            )
            .map_err(|e| {
                if e.code() == ErrorCode::ResourceLimit {
                    Failure::Rejected(Rejection::ResourceLimit)
                } else {
                    Failure::Constructor(e)
                }
            })?;
            let handle = DomainHandle {
                registry: domains.identity,
                sequence,
                writable: true,
            };
            domains.entries.insert(
                sequence,
                Domain {
                    root: root.map(std::sync::Arc::new),
                    module: module.clone(),
                    source_module,
                    source_token: token,
                    ty: info.signature.ret,
                    code: 0,
                    revision: 0,
                    schema,
                    failure: None,
                    closing: false,
                    tasks: HashMap::new(),
                    automatic: Vec::new(),
                    limits: pair.limits,
                },
            );
            domains.next_domain = next;
            publication.set_target(handle, token * 2 + 1);
            staged.roots.push(sequence);
            staged.publications.push(publication);
            selection.handles[index] = handle;
        }
        Ok(())
    })();
    if let Err(error) = result {
        staged.discard(domains);
        return Err(error);
    }
    pair.words.clear();
    pair.words.resize(selection.handles.len(), 0);
    pair.words.extend(input);
    pair.handles = selection.handles;
    pair.method = method;
    pair.info = info;
    Ok(staged)
}

pub(in super::super) struct Packet {
    pub source_key: usize,
    pub descriptors: Vec<u64>,
    pub keys: Vec<String>,
}
impl From<Vec<u64>> for Packet {
    fn from(descriptors: Vec<u64>) -> Self {
        Self {
            descriptors,
            source_key: 0,
            keys: vec![],
        }
    }
}
pub(in super::super) fn decode(words: &[u64], source_key: usize) -> Result<Packet, Error> {
    let steps = words[0] as usize;
    let count = words[steps] as usize;
    let names = words[1] as usize;
    let names_count = words[names] as usize;
    let mut keys = Vec::with_capacity(names_count);
    for offset in &words[names + 1..names + 1 + names_count] {
        let offset = *offset as usize;
        let length = words[offset] as usize;
        let mut bytes = Vec::with_capacity(length);
        for word in &words[offset + 1..offset + 1 + length.div_ceil(8)] {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        bytes.truncate(length);
        keys.push(String::from_utf8(bytes).map_err(|_| invalid("Invalid directory name UTF-8"))?);
    }
    Ok(Packet {
        source_key,
        descriptors: words[steps + 1..steps + 1 + count].to_vec(),
        keys,
    })
}
