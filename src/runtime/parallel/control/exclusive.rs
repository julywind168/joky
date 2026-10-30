//! A whole-loan permission, independent of per-operation ResourceLease. Only
//! Mailbox receiver identities qualify; queue state stays on the control thread.
use super::*;

pub(super) struct Return {
    source: Arc<Proxy>,
    armed: bool,
}
impl Drop for Return {
    fn drop(&mut self) {
        if self.armed {
            self.source.exclusive.store(false, Ordering::Release);
        }
    }
}

pub(in crate::runtime::parallel) struct ExclusiveQueue(Arc<Proxy>);
impl ExclusiveQueue {
    unsafe fn source(bits: u64) -> Result<Arc<Proxy>, Error> {
        let buffer = (bits as *const Buffer)
            .as_ref()
            .ok_or_else(|| invalid("Null Mailbox loan"))?;
        if !buffer.scoped_owned_resource() {
            return Err(invalid(
                "Mailbox loan requires a current-task owning source",
            ));
        }
        let source = proxy(bits, &pool::control()?)?;
        if !source.mailbox || source.loan.is_some() || source.core.closed.load(Ordering::Acquire) {
            return Err(invalid("Resource is not an available Mailbox receiver"));
        }
        Ok(source)
    }
    pub(in crate::runtime::parallel) unsafe fn eligible(bits: u64) -> bool {
        Self::source(bits).is_ok()
    }
    pub(in crate::runtime::parallel) unsafe fn capture(bits: u64) -> Result<Self, Error> {
        let source = Self::source(bits)?;
        // Reserve all handoff and return metadata before denying source access.
        // Drop returns permission without allocating, including failed install.
        let charge = source.core.root.accounts().reserve(
            std::mem::size_of::<Proxy>()
                + std::mem::size_of::<Return>()
                + 4 * std::mem::size_of::<usize>(),
        )?;
        let mut returned = Arc::new(Return {
            source: source.clone(),
            armed: false,
        });
        source
            .exclusive
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| invalid("Mailbox receiver is already borrowed"))?;
        Arc::get_mut(&mut returned).unwrap().armed = true;
        Ok(Self(Arc::new(Proxy {
            id: source.id,
            shared_vm: false,
            mailbox: true,
            exclusive: AtomicBool::new(false),
            loan: Some(returned),
            core: source.core.clone(),
            _charge: charge,
        })))
    }
    pub(in crate::runtime::parallel) fn install(self) -> Result<OwnedResource, Error> {
        // Pending control calls retain this exact proxy too. Permission returns
        // only after both the receiver view and every request have stopped.
        proxy_resource(self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exclusive_mailbox_permission_rejects_aliases_and_returns_without_budget() {
        struct Probe(crate::runtime::parallel::TaskBudget);
        impl pool::Job for Probe {
            fn poll(&mut self) -> std::task::Poll<Result<pool::Output, Error>> {
                let result = (|| unsafe {
                    let root = crate::runtime::parallel::current().unwrap();
                    let core = pool::control()?;
                    let mut arena = Arena::with_storage(root.clone());
                    let vm = create_vm(&mut arena, 1)?.as_ptr() as u64;
                    assert!(!ExclusiveQueue::eligible(vm));
                    Buffer::release(&mut arena, vm as *mut Buffer)?;
                    let request = core.submit(
                        Operation::Domain(crate::runtime::domain::Operation::OutboxCreate),
                        vec![Value::Word(1), Value::Word(0)],
                        root.accounts().reserve(std::mem::size_of::<Request>())?,
                    )?;
                    let mut state = lock(&request.state);
                    while state.result.is_none() {
                        state = request.done.wait(state).unwrap();
                    }
                    let Value::Resource(owner) = state.result.take().unwrap()?.value else {
                        unreachable!()
                    };
                    drop(state);
                    drop(request);
                    let bits = adopt(&mut arena, owner.clone())?;
                    assert!(ExclusiveQueue::eligible(bits));
                    let captured = ExclusiveQueue::capture(bits)?;
                    assert!(proxy(bits, &core).is_err());
                    assert!(ExclusiveQueue::capture(bits).is_err());
                    let owner_token = (&*(bits as *const Buffer))
                        .resource_ref()?
                        .control_token(release)?;
                    assert!(!transferable(NonNull::new(owner_token).unwrap(), release));
                    let alias = Buffer::external(&mut arena, captured.install()?)?.as_ptr() as u64;
                    assert!(proxy(alias, &core).is_ok());
                    assert!(!ExclusiveQueue::eligible(alias));
                    let alias_token = (&*(alias as *const Buffer))
                        .resource_ref()?
                        .control_token(release)?;
                    assert!(!transferable(NonNull::new(alias_token).unwrap(), release));
                    // This is the same retention as queued input / Call.keep.
                    let pending = proxy(alias, &core)?;
                    Buffer::release(&mut arena, alias as *mut Buffer)?;
                    assert!(proxy(bits, &core).is_err());
                    drop(pending);
                    assert!(proxy(bits, &core).is_ok());
                    assert!(transferable(NonNull::new(owner_token).unwrap(), release));
                    // A return never needs a new allocation, including install failure.
                    let captured = ExclusiveQueue::capture(bits)?;
                    let held = root.accounts().reserve((32 << 20) - self.0.used())?;
                    // An independent zero-budget destination makes failure
                    // deterministic even if the control thread refunds earlier replies.
                    let mut destination = Arena::with_budget(
                        Arc::new(crate::runtime::arena::Budget::default()),
                        None,
                    );
                    let installed = captured
                        .install()
                        .and_then(|resource| Buffer::external(&mut destination, resource));
                    assert!(installed.is_err());
                    assert!(proxy(bits, &core).is_ok());
                    drop(held);
                    Buffer::release(&mut arena, bits as *mut Buffer)?;
                    drop(owner);
                    Ok(Box::new(()) as pool::Output)
                })();
                std::task::Poll::Ready(result)
            }
        }
        let budget = crate::runtime::parallel::TaskBudget::new(32 << 20);
        let mut executor = pool::ParallelExecutor::new(1, 8).unwrap();
        let probe_budget = budget.clone();
        let task = executor
            .submit(
                Storage::new(vec![budget.clone()]).unwrap(),
                Arc::new(()),
                Box::new(move || Ok(Box::new(Probe(probe_budget)))),
            )
            .unwrap();
        task.wait().unwrap();
        task.take().unwrap().unwrap();
        drop(task);
        executor.wait_closed();
        assert_eq!(budget.used(), 0);
    }
}
