//! A direct constructor builds a private root on a worker. Only the control
//! thread can install the completed root and issue its owner.
use super::*;
use crate::runtime::{buffer::Buffer, domain::Operation};

struct Constructor {
    code: Code,
    info: FunctionInfo,
    words: Vec<u64>,
    limits: InterpreterLimits,
    budget: Arc<Budget>,
    _charge: reads::Charge,
    // A cancelled call can retire its descriptor wrapper before this poll ends.
    _program: Arc<dyn std::any::Any + Send + Sync>,
}
impl pool::Job for Constructor {
    fn poll(&mut self) -> Poll<Result<pool::Output, Error>> {
        Poll::Ready(
            Root::construct_with(
                &self.info,
                &self.words,
                self.budget.clone(),
                |input, arena| self.code.invoke(input, self.limits, arena),
            )
            .map(|root| {
                #[cfg(test)]
                let root = {
                    let mut root = root;
                    root.untrack();
                    root
                };
                Box::new(root) as pool::Output
            }),
        )
    }
}
pub(crate) struct Pending {
    job: pool::Handle,
    module: LoadedModule,
    key: usize,
    ty: Type,
}
impl Drop for Pending {
    fn drop(&mut self) {
        if !self.job.cleaned() {
            self.job.cancel();
        }
    }
}
/// The frame has the compiler's Create or CreateValues layout and the call's
/// frozen program is installed on the control thread for this poll.
pub(crate) unsafe fn poll(
    pending: &mut Option<Pending>,
    arena: &mut Arena,
    frame: &mut CpsFrame,
    op: Operation,
    program: &Arc<dyn std::any::Any + Send + Sync>,
    caller: &pool::DomainSpawner,
) -> Result<Option<u64>, Error> {
    let registry = crate::runtime::executor::current_executor()
        .ok_or_else(|| invalid("Domain creation requires an executor"))?
        .domains();
    let limits = crate::runtime::suspension::current_limits();
    if pending.is_none() {
        let (module, key) = source::module(&registry, frame.slots[0])?;
        let name = crate::runtime::owned_text::borrow(frame.slots[1] as *const Buffer)?;
        let info = module.instance_info(name)?;
        if timers::automatic::has_declarations(&module, &info.signature.ret) {
            return Err(invalid("Periodic domain creation requires Local execution"));
        }
        let budget = registry.borrow().creation_budget()?;
        let words = source::constructor_words(&info, op, frame.slots[2])?;
        let ty = info.signature.ret.clone();
        let constructor = Constructor {
            code: Code::freeze(&module, name)?,
            info,
            limits,
            _charge: reads::Charge::new(
                budget.clone(),
                std::mem::size_of::<Constructor>() + words.capacity() * 8,
            )?,
            budget,
            words,
            _program: program.clone(),
        };
        // The constructor is a child of this caller, including when a race
        // cancels just this branch and the outer root continues serving requests.
        let wake = current_waker().ok_or_else(|| invalid("Missing domain control waker"))?;
        let job = caller.submit(Box::new(move || Ok(Box::new(constructor))))?;
        job.observe(wake);
        *pending = Some(Pending {
            job,
            module,
            key,
            ty,
        });
    }
    let pending = pending.as_ref().unwrap();
    if !pending.job.cleaned() {
        frame.wake = None;
        return Ok(None);
    }
    let root = pending
        .job
        .take()
        .ok_or_else(|| invalid("Missing domain constructor result"))??
        .downcast::<Root>()
        .map_err(|_| invalid("Invalid domain constructor result"))?;
    // Concurrent native constructors can decode the same descriptor before
    // either root exists. Publish both under the first installed code identity.
    let module = registry
        .borrow()
        .source_module(pending.key)
        .unwrap_or_else(|| pending.module.clone());
    let domain = insert_root(&registry, module, pending.ty.clone(), limits, *root)?;
    registry
        .borrow_mut()
        .entries
        .get_mut(&domain.handle.sequence)
        .unwrap()
        .source_module = pending.key;
    source::adopt(arena, domain).map(Some)
}
