//! Unpublished roots for compiler-generated business operations.
use super::*;

pub(super) struct Candidates {
    roots: Vec<(u64, Type, Root)>,
    _charge: reads::Charge,
}
impl Candidates {
    pub(super) fn capture(
        domains: &Domains,
        handles: &[DomainHandle],
        mutable: &[bool],
    ) -> Result<Self, Error> {
        let mut roots = Vec::new();
        roots
            .try_reserve_exact(mutable.iter().filter(|write| **write).count())
            .map_err(|_| limited("Transaction root allocation failed"))?;
        let charge = reads::Charge::new(
            domains.budget.clone(),
            roots.capacity() * std::mem::size_of::<(u64, Type, Root)>(),
        )?;
        for (handle, write) in handles.iter().zip(mutable) {
            if !write {
                continue;
            }
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
        domains: &mut Domains,
        pair: &mut Pair,
        module: &LoadedModule,
        mutable: &[bool],
        entered: &mut bool,
    ) -> Result<Vec<u64>, Error> {
        let budget = domains.budget.clone();
        let mut roots = BorrowedRoots {
            candidates: self,
            domains,
        };
        if pair.handles.len() == 1 {
            let (ty, root) = storage::RootAccess::root(&mut roots, pair.handles[0].sequence);
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
            &mut roots,
            &pair.handles,
            module,
            &pair.method,
            &mut pair.words,
            (mutable, entered),
            (pair.limits, budget),
        )
    }

    pub(super) fn publish(self, domains: &mut Domains) {
        // Capture, execution and publication share a single synchronous FIFO
        // turn. No host callbacks or scheduler yield can change the read set.
        // All fallible graph/result preparation has finished before this loop.
        for (id, _, root) in self.roots {
            domains.entries.get_mut(&id).unwrap().root = Some(root);
        }
    }
}

// This view cannot escape the synchronous registry borrow. Only writable roots
// are candidates; shared roots stay live and use the existing graph restoration
// path on both success and failure. Code qualification forbids writes to them.
struct BorrowedRoots<'a> {
    candidates: &'a mut Candidates,
    domains: &'a mut Domains,
}
impl storage::RootAccess for BorrowedRoots<'_> {
    fn root(&mut self, sequence: u64) -> (&Type, &mut Root) {
        if let Some((_, ty, root)) = self
            .candidates
            .roots
            .iter_mut()
            .find(|(id, _, _)| *id == sequence)
        {
            (ty, root)
        } else {
            storage::RootAccess::root(self.domains, sequence)
        }
    }
}

pub(super) fn accepts(ty: &Type, words: &[u64]) -> bool {
    // Only the standard top-level Result controls publication. Bool, Option,
    // nested Result and user enums (even with an Err variant) are ordinary values.
    !matches!(ty, Type::Enum(e) if e.name == "joky/result.Result" && words.first() == Some(&1))
}

#[cfg(test)]
#[path = "transactions_tests.rs"]
mod tests;
