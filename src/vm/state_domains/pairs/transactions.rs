//! Unpublished roots for compiler-generated business operations.
use super::*;

pub(super) struct Candidates {
    roots: Vec<(u64, Type, Root)>,
    _charge: reads::Charge,
}
impl Candidates {
    pub(super) fn capture(domains: &Domains, handles: &[DomainHandle]) -> Result<Self, Error> {
        let mut roots = Vec::new();
        roots
            .try_reserve_exact(handles.len())
            .map_err(|_| limited("Transaction root allocation failed"))?;
        let charge = reads::Charge::new(
            domains.budget.clone(),
            roots.capacity() * std::mem::size_of::<(u64, Type, Root)>(),
        )?;
        for handle in handles {
            if roots.iter().any(|(id, _, _)| *id == handle.sequence) {
                continue;
            }
            let domain = domains.get(*handle)?;
            roots.push((
                handle.sequence,
                domain.ty.clone(),
                domain
                    .root
                    .as_ref()
                    .unwrap()
                    .snapshot(&domain.ty, domains.budget.clone())?,
            ));
        }
        Ok(Self {
            roots,
            _charge: charge,
        })
    }

    pub(super) fn invoke(
        &mut self,
        domains: &Domains,
        pair: &mut Pair,
        module: &LoadedModule,
        mutable: &[bool],
        entered: &mut bool,
    ) -> Result<Vec<u64>, Error> {
        if pair.handles.len() == 1 {
            let (_, ty, root) = &mut self.roots[0];
            return root.invoke_values(
                module,
                ty,
                &pair.method,
                &pair.info,
                (&mut pair.words, Some(entered)),
                pair.limits,
            );
        }
        Root::invoke_roots(
            self,
            &pair.handles,
            module,
            &pair.method,
            &mut pair.words,
            (mutable, entered),
            (pair.limits, domains.budget.clone()),
        )
    }

    pub(super) fn publish(self, domains: &mut Domains, handles: &[DomainHandle], mutable: &[bool]) {
        // Capture, execution and publication share a single synchronous FIFO
        // turn. No host callbacks or scheduler yield can change the read set.
        // All fallible graph/result preparation has finished before this loop.
        for (id, _, root) in self.roots {
            let index = handles.iter().position(|h| h.sequence == id).unwrap();
            if mutable[index] {
                domains.entries.get_mut(&id).unwrap().root = Some(root);
            }
        }
    }
}

impl storage::RootAccess for Candidates {
    fn root(&mut self, sequence: u64) -> (&Type, &mut Root) {
        let (_, ty, root) = self
            .roots
            .iter_mut()
            .find(|(id, _, _)| *id == sequence)
            .unwrap();
        (ty, root)
    }
}

pub(super) fn accepts(ty: &Type, words: &[u64]) -> bool {
    // Only the standard top-level Result controls publication. Bool, Option,
    // nested Result and user enums (even with an Err variant) are ordinary values.
    !matches!(ty, Type::Enum(e) if e.name == "joky/result.Result" && words.first() == Some(&1))
}
