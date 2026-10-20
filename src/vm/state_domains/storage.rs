//! Hook-free owning roots. Invocation garbage never becomes persistent state.
use super::*;
use crate::runtime::{arena::ExternalCharge, buffer::Buffer};

pub(super) struct Root {
    _charge: ExternalCharge,
    arena: Arena,
    pointer: NonNull<u8>,
    owners: Vec<usize>,
}
impl Root {
    pub(super) fn construct(
        module: &LoadedModule,
        info: &FunctionInfo,
        constructor: &str,
        words: &[u64],
        limits: InterpreterLimits,
        budget: Rc<Budget>,
    ) -> Result<Self, Error> {
        let ty = &info.signature.ret;
        let mut arena = Arena::with_budget(budget, None);
        let layout = ty.storage_layout().map_err(|e| invalid(&e.to_string()))?;
        let owners: Vec<_> = ty
            .storage_scalars()
            .map_err(|e| invalid(&e.to_string()))?
            .into_iter()
            .filter(|(_, ty)| ty.is_buffer())
            .map(|(offset, _)| offset)
            .collect();
        let charge = arena.reserve_external(owners.len() * std::mem::size_of::<usize>())?;
        let pointer = arena.allocate(layout.size, layout.align)?;
        unsafe {
            pointer.as_ptr().write_bytes(0, layout.size.max(1));
        }
        let mut root = Self {
            _charge: charge,
            arena,
            pointer,
            owners,
        };
        let input = decode_input(info, words, &mut root.arena)?;
        let result = module.invoke_instance(constructor, &input, limits, &mut root.arena)?;
        super::super::instances::initialize(root.pointer, ty, &result)?;
        let live = root.allocations()?;
        unsafe {
            root.arena.retain_allocations(&live);
        }
        Ok(root)
    }
    pub(super) fn snapshot(&self, ty: &Type, budget: Rc<Budget>) -> Result<Self, Error> {
        let mut arena = Arena::with_budget(budget, None);
        let layout = ty.storage_layout().map_err(|e| invalid(&e.to_string()))?;
        let charge = arena.reserve_external(self.owners.len() * std::mem::size_of::<usize>())?;
        let mut owners = Vec::new();
        owners
            .try_reserve_exact(self.owners.len())
            .map_err(|_| limited("Root descriptor allocation failed"))?;
        owners.extend_from_slice(&self.owners);
        let pointer = arena.allocate(layout.size, layout.align)?;
        unsafe {
            std::ptr::copy_nonoverlapping(self.pointer.as_ptr(), pointer.as_ptr(), layout.size);
            for &offset in &owners {
                pointer
                    .as_ptr()
                    .add(offset)
                    .cast::<*mut Buffer>()
                    .write(std::ptr::null_mut());
            }
        }
        let mut root = Self {
            _charge: charge,
            arena,
            pointer,
            owners,
        };
        let originals = self.owner_pointers()?;
        let copies = unsafe { Buffer::copy_owners(&self.arena, &originals, &mut root.arena)? };
        for (&offset, copy) in root.owners.iter().zip(copies) {
            unsafe {
                root.pointer
                    .as_ptr()
                    .add(offset)
                    .cast::<*mut Buffer>()
                    .write(copy);
            }
        }
        Ok(root)
    }
    fn owner_pointers(&self) -> Result<Vec<*mut Buffer>, Error> {
        let mut owners = Vec::new();
        owners
            .try_reserve_exact(self.owners.len())
            .map_err(|_| limited("Root graph allocation failed"))?;
        owners.extend(self.owners.iter().map(|&offset| unsafe {
            self.pointer
                .as_ptr()
                .add(offset)
                .cast::<*mut Buffer>()
                .read()
        }));
        Ok(owners)
    }
    // One-field owning wrappers borrow the buffer capability; mixed aggregates
    // borrow their inline root storage. Both remain owned by this Root.
    pub(super) fn borrow_pointer(&self, ty: &Type) -> u64 {
        if ty.is_owner() && !ty.is_inline_owner() {
            unsafe { self.pointer.as_ptr().cast::<usize>().read() as u64 }
        } else {
            self.pointer.as_ptr() as u64
        }
    }
    pub(super) fn scalar_pointer(&self) -> u64 {
        self.pointer.as_ptr() as u64
    }
    fn allocations(&self) -> Result<Vec<NonNull<u8>>, Error> {
        let mut graph = unsafe {
            Buffer::owning_allocations(
                &self.arena,
                self.owners.iter().map(|offset| {
                    self.pointer
                        .as_ptr()
                        .add(*offset)
                        .cast::<*mut Buffer>()
                        .read()
                }),
            )
        }?;
        graph
            .allocations
            .try_reserve(1)
            .map_err(|_| limited("Domain root graph allocation failed"))?;
        graph.allocations.push(self.pointer);
        Ok(graph.allocations)
    }
    /// Import all distinct owning graphs for one synchronous multi-root turn.
    /// Read-only graphs are prepared before entry so they can always be restored,
    /// even when graph discovery or export reservation fails after a write.
    pub(super) fn invoke_many(
        domains: &mut Domains,
        handles: &[DomainHandle],
        module: &LoadedModule,
        method: &str,
        words: &mut [u64],
        access: (&[bool], &mut bool),
        limits: InterpreterLimits,
    ) -> Result<Vec<u64>, Error> {
        let (mutable, entered) = access;
        let info = module.instance_info(method)?;
        let mut arena = Arena::with_budget(domains.budget.clone(), None);
        let mut graphs = Vec::new();
        graphs
            .try_reserve_exact(handles.len())
            .map_err(|_| limited("Domain graph allocation failed"))?;
        let mut members = Vec::new();
        members
            .try_reserve_exact(handles.len())
            .map_err(|_| limited("Domain graph allocation failed"))?;
        for (index, handle) in handles.iter().enumerate() {
            let domain = &domains.entries[&handle.sequence];
            let root = domain.root.as_ref().unwrap();
            words[index] = root.borrow_pointer(&domain.ty);
            if members
                .iter()
                .any(|&i: &usize| handles[i].sequence == handle.sequence)
            {
                continue;
            }
            let graph = unsafe { Buffer::owning_allocations(&root.arena, root.owner_pointers()?)? };
            members.push(index);
            graphs.push(graph);
        }
        let scratch_bytes = graphs
            .iter()
            .map(|g| {
                g.allocations.capacity() * std::mem::size_of::<NonNull<u8>>()
                    + g.buffers.capacity() * std::mem::size_of::<NonNull<Buffer>>()
            })
            .sum::<usize>();
        let _scratch = arena.reserve_external(scratch_bytes)?;
        arena.reserve_transfer(graphs.iter().map(|g| g.allocations.len()).sum())?;
        for (&index, graph) in members.iter().zip(&graphs) {
            let root = domains
                .entries
                .get_mut(&handles[index].sequence)
                .unwrap()
                .root
                .as_mut()
                .unwrap();
            root.arena.transfer_reserved(&mut arena, &graph.allocations);
            unsafe {
                Buffer::migrate_metadata(&graph.buffers, &arena);
            }
        }
        let mut result = decode_input(&info, words, &mut arena)
            .and_then(|input| {
                *entered = true;
                module.invoke_instance(method, &input, limits, &mut arena)
            })
            .and_then(|raw| encode_output(&info.signature.ret, raw, &mut arena));
        let mut after = Vec::new();
        let prepared = (|| {
            if result.is_err() {
                return Ok(None);
            }
            after
                .try_reserve_exact(members.len())
                .map_err(|_| limited("Domain graph allocation failed"))?;
            for &index in &members {
                let root = domains
                    .entries
                    .get_mut(&handles[index].sequence)
                    .unwrap()
                    .root
                    .as_mut()
                    .unwrap();
                let graph = unsafe { Buffer::owning_allocations(&arena, root.owner_pointers()?)? };
                if after.iter().any(|g: &crate::runtime::buffer::OwningGraph| {
                    g.buffers
                        .iter()
                        .any(|buffer| graph.buffers.contains(buffer))
                }) {
                    return Err(invalid("Aliased domain owning graphs"));
                }
                root.arena.reserve_transfer(graph.allocations.len())?;
                after.push(graph);
            }
            let bytes = after
                .iter()
                .map(|g| {
                    g.allocations.capacity() * std::mem::size_of::<NonNull<u8>>()
                        + g.buffers.capacity() * std::mem::size_of::<NonNull<Buffer>>()
                })
                .sum();
            Ok::<_, Error>(Some(arena.reserve_external(bytes)?))
        })();
        let _export_charge = match prepared {
            Ok(charge) => charge,
            Err(error) => {
                result = Err(error);
                None
            }
        };
        if result.is_ok() {
            for (&index, graph) in members.iter().zip(&after) {
                let root = domains
                    .entries
                    .get_mut(&handles[index].sequence)
                    .unwrap()
                    .root
                    .as_mut()
                    .unwrap();
                arena.transfer_reserved(&mut root.arena, &graph.allocations);
                unsafe {
                    Buffer::migrate_metadata(&graph.buffers, &root.arena);
                }
            }
        } else {
            // Shared graphs cannot change, and their original arena capacities
            // survived import. Restoration cannot allocate or fail.
            for (&index, graph) in members.iter().zip(&graphs) {
                let root = domains
                    .entries
                    .get_mut(&handles[index].sequence)
                    .unwrap()
                    .root
                    .as_mut()
                    .unwrap();
                if !*entered || !mutable[index] {
                    arena.transfer_reserved(&mut root.arena, &graph.allocations);
                    unsafe {
                        Buffer::migrate_metadata(&graph.buffers, &root.arena);
                    }
                }
            }
            for &index in &members {
                if *entered && mutable[index] {
                    let root = domains
                        .entries
                        .get_mut(&handles[index].sequence)
                        .unwrap()
                        .root
                        .as_mut()
                        .unwrap();
                    root.release_owners(&mut arena);
                }
            }
        }
        result
    }
    fn release_owners(&mut self, arena: &mut Arena) {
        for &offset in &self.owners {
            unsafe {
                let lane = self.pointer.as_ptr().add(offset).cast::<*mut Buffer>();
                let owner = lane.read();
                lane.write(std::ptr::null_mut());
                if !owner.is_null() {
                    let _ = Buffer::release(arena, owner);
                }
            }
        }
    }
    pub(super) fn invoke(
        &mut self,
        module: &LoadedModule,
        ty: &Type,
        method: &str,
        info: &FunctionInfo,
        words: &mut [u64],
        limits: InterpreterLimits,
    ) -> Result<InstanceValue, Error> {
        let words = self.invoke_values(module, ty, method, info, (words, None), limits)?;
        InstanceValue::from_bits(&info.signature.ret, words.first().copied().unwrap_or(0))
            .ok_or_else(|| invalid("Invalid domain scalar result"))
    }
    pub(super) fn invoke_values(
        &mut self,
        module: &LoadedModule,
        ty: &Type,
        method: &str,
        info: &FunctionInfo,
        input: (&mut [u64], Option<&mut bool>),
        limits: InterpreterLimits,
    ) -> Result<Vec<u64>, Error> {
        let (words, entered) = input;
        let mutable = info.signature.params[0].reference_type().unwrap().mutable;
        // A read cannot change the graph. Prepare its live set before entering
        // user code so even an allocation failure can reclaim all temporaries.
        let before = if mutable
            && !info
                .signature
                .params
                .iter()
                .any(crate::runtime::domain_packet::has_owners)
        {
            None
        } else {
            Some(self.allocations()?)
        };
        words[0] = self.borrow_pointer(ty);
        let input = match decode_input(info, words, &mut self.arena) {
            Ok(input) => input,
            Err(error) => {
                if let Some(live) = before {
                    unsafe {
                        self.arena.retain_allocations(&live);
                    }
                }
                return Err(error);
            }
        };
        if let Some(entered) = entered {
            *entered = true;
        }
        let result = module
            .invoke_instance(method, &input, limits, &mut self.arena)
            .and_then(|raw| encode_output(&info.signature.ret, raw, &mut self.arena));
        if !mutable {
            unsafe {
                self.arena.retain_allocations(before.as_ref().unwrap());
            }
        } else if result.is_ok() {
            let live = self.allocations()?;
            unsafe {
                self.arena.retain_allocations(&live);
            }
        }
        result
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        // Compiler-authored descriptors recursively release child owners.
        // Eligibility excludes user callbacks throughout the storage graph.
        for &offset in &self.owners {
            unsafe {
                let owner = self
                    .pointer
                    .as_ptr()
                    .add(offset)
                    .cast::<*mut Buffer>()
                    .read();
                if !owner.is_null() {
                    let _ = Buffer::release(&mut self.arena, owner);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;

fn decode_input<'a>(
    info: &FunctionInfo,
    words: &'a [u64],
    arena: &mut Arena,
) -> Result<std::borrow::Cow<'a, [u64]>, Error> {
    let count = crate::core::state_domain::access_count(info);
    let types = &info.signature.params[count..];
    if !types.iter().any(crate::runtime::domain_packet::has_owners) {
        return Ok(std::borrow::Cow::Borrowed(words));
    }
    let mut input = words[..count].to_vec();
    input.extend(crate::runtime::domain_packet::decode(
        types,
        &words[count..],
        arena,
    )?);
    Ok(std::borrow::Cow::Owned(input))
}
fn encode_output(ty: &Type, raw: Vec<u64>, arena: &mut Arena) -> Result<Vec<u64>, Error> {
    if crate::runtime::domain_packet::has_owners(ty) {
        unsafe { crate::runtime::domain_packet::encode_result(ty, &raw, arena) }
    } else {
        values::result_words(ty, raw)
    }
}
