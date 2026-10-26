//! A grant pins shared roots; only private candidates move into the invocation arena.
use super::*;
use std::sync::Arc;

// A Root is a closed owning graph: the arena's local counters are shared only
// with its own ExternalCharge and buffers. No counter Rc escapes the root.
// The persistent budget is atomic. Eligibility excludes resources, closures and
// user Drop. Moving the complete graph therefore moves every local counter alias.
unsafe impl Send for Root {}
// Shared access exposes immutable root bytes only. Mutation requires &mut Root;
// live registry slots use Arc::get_mut, and grants pin every shared participant.
unsafe impl Sync for Root {}

pub(in super::super) enum InputRoot {
    Read(Arc<Root>),
    Write(Arc<Root>),
    New,
    Alias(usize),
}
impl InputRoot {
    fn root(&self) -> &Root {
        match self {
            Self::Read(root) => root,
            Self::Write(root) => root,
            _ => unreachable!(),
        }
    }
}
pub(in super::super) struct Creation {
    pub id: u64,
    pub code: super::super::parallel::Code,
    pub info: FunctionInfo,
    pub words: Vec<u64>,
}
pub(in super::super) struct Computation {
    pub charge: reads::Charge,
    pub creations: Vec<Creation>,
    pub roots: Vec<(u64, Type, InputRoot)>,
    pub words: Vec<u64>,
    pub info: FunctionInfo,
    pub limits: InterpreterLimits,
    pub code: super::super::parallel::Code,
    pub budget: std::sync::Arc<Budget>,
}
impl Computation {
    pub fn invoke(&mut self) -> Result<Vec<u64>, Error> {
        for creation in self.creations.drain(..) {
            let root = Root::construct_with(
                &creation.info,
                &creation.words,
                self.budget.clone(),
                |input, arena| creation.code.invoke(input, self.limits, arena),
            )?;
            #[cfg(test)]
            let root = {
                let mut root = root;
                root.untrack();
                root
            };
            let index = self
                .roots
                .iter()
                .position(|(id, _, _)| *id == creation.id)
                .unwrap();
            self.roots[index].2 = InputRoot::Write(Arc::new(root));
            for (other, (id, _, value)) in self.roots.iter_mut().enumerate() {
                if other != index && *id == creation.id {
                    *value = InputRoot::Alias(index);
                }
            }
        }
        let mut arena = Arena::with_budget(self.budget.clone(), None);
        #[cfg(test)]
        arena.untrack();
        let mut graphs = Vec::new();
        for (index, (_, ty, root)) in self.roots.iter().enumerate() {
            self.words[index] = match root {
                InputRoot::Alias(previous) => self.roots[*previous].2.root().borrow_pointer(ty),
                _ => root.root().borrow_pointer(ty),
            };
            if let InputRoot::Write(root) = root {
                graphs.push((index, unsafe {
                    Buffer::owning_allocations(&root.arena, root.owner_pointers()?)?
                }));
            }
        }
        let _scratch = arena.reserve_external(
            graphs
                .iter()
                .map(|(_, g)| {
                    g.allocations.capacity() * std::mem::size_of::<NonNull<u8>>()
                        + g.buffers.capacity() * std::mem::size_of::<NonNull<Buffer>>()
                })
                .sum(),
        )?;
        arena.reserve_transfer(graphs.iter().map(|(_, g)| g.allocations.len()).sum())?;
        for (index, graph) in &graphs {
            let InputRoot::Write(root) = &mut self.roots[*index].2 else {
                unreachable!()
            };
            let root = Arc::get_mut(root).expect("exclusive write candidate");
            root.arena.transfer_reserved(&mut arena, &graph.allocations);
            unsafe {
                Buffer::migrate_metadata(&graph.buffers, &arena);
            }
        }
        let mut result = decode_input(&self.info, &self.words, &mut arena)
            .and_then(|input| self.code.invoke(&input, self.limits, &mut arena))
            .and_then(|raw| encode_output(&self.info.signature.ret, raw, &mut arena));
        let mut after = Vec::new();
        let prepare = (|| {
            if result.is_err() {
                return Ok(None);
            }
            after
                .try_reserve_exact(graphs.len())
                .map_err(|_| limited("Domain graph allocation failed"))?;
            for (index, _) in &graphs {
                let InputRoot::Write(root) = &mut self.roots[*index].2 else {
                    unreachable!()
                };
                let root = Arc::get_mut(root).expect("exclusive write candidate");
                let graph = unsafe { Buffer::owning_allocations(&arena, root.owner_pointers()?)? };
                if after.iter().any(|g: &crate::runtime::buffer::OwningGraph| {
                    g.buffers.iter().any(|p| graph.buffers.contains(p))
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
        let _export = match prepare {
            Ok(charge) => charge,
            Err(error) => {
                result = Err(error);
                None
            }
        };
        for ((index, _), graph) in graphs.iter().zip(&after) {
            if result.is_ok() {
                let InputRoot::Write(root) = &mut self.roots[*index].2 else {
                    unreachable!()
                };
                let root = Arc::get_mut(root).expect("exclusive write candidate");
                arena.transfer_reserved(&mut root.arena, &graph.allocations);
                unsafe {
                    Buffer::migrate_metadata(&graph.buffers, &root.arena);
                }
            }
        }
        if result.is_err() {
            for (index, _) in &graphs {
                let InputRoot::Write(root) = &mut self.roots[*index].2 else {
                    unreachable!()
                };
                let root = Arc::get_mut(root).expect("exclusive write candidate");
                root.release_owners(&mut arena);
            }
        }
        self.words[..self.roots.len()].fill(0);
        result
    }
}
