//! Flat, hook-free owning roots. Invocation garbage never becomes persistent state.
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
        let result = module.invoke_instance(constructor, words, limits, &mut root.arena)?;
        super::super::instances::initialize(root.pointer, ty, &result)?;
        let live = root.allocations()?;
        unsafe {
            root.arena.retain_allocations(&live);
        }
        Ok(root)
    }
    pub(super) fn snapshot_scalar(&self, ty: &Type, budget: Rc<Budget>) -> Result<Self, Error> {
        if !matches!(ty, Type::Record(r) if r.fields.iter().all(|f| instance::scalar(&f.ty))) {
            return Err(invalid("Access sets require scalar record roots"));
        }
        let mut arena = Arena::with_budget(budget, None);
        let layout = ty.storage_layout().map_err(|e| invalid(&e.to_string()))?;
        let pointer = arena.allocate(layout.size, layout.align)?;
        unsafe {
            std::ptr::copy_nonoverlapping(self.pointer.as_ptr(), pointer.as_ptr(), layout.size);
        }
        let charge = arena.reserve_external(0)?;
        Ok(Self {
            _charge: charge,
            arena,
            pointer,
            owners: Vec::new(),
        })
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
    pub(super) fn invoke(
        &mut self,
        module: &LoadedModule,
        ty: &Type,
        method: &str,
        info: &FunctionInfo,
        words: &mut [u64],
        limits: InterpreterLimits,
    ) -> Result<InstanceValue, Error> {
        let mutable = info.signature.params[0].reference_type().unwrap().mutable;
        // A read cannot change the graph. Prepare its live set before entering
        // user code so even an allocation failure can reclaim all temporaries.
        let before = if mutable {
            None
        } else {
            Some(self.allocations()?)
        };
        words[0] = if ty.is_owner() && !ty.is_inline_owner() {
            unsafe { self.pointer.as_ptr().cast::<usize>().read() as u64 }
        } else {
            self.pointer.as_ptr() as u64
        };
        let result = module
            .invoke_instance(method, words, limits, &mut self.arena)
            .and_then(|words| {
                InstanceValue::from_bits(&info.signature.ret, words.first().copied().unwrap_or(0))
                    .ok_or_else(|| invalid("Invalid domain scalar result"))
            });
        if let Some(live) = before {
            unsafe {
                self.arena.retain_allocations(&live);
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
        // Eligibility admits only String and scalar buffers: no callback or
        // recursively owning element can execute during root destruction.
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
