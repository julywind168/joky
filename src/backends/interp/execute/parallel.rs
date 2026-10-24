//! Transfer only checked scalars and exclusive text; construct activations locally.
use super::*;
use crate::runtime::parallel::{pool, Lease, OwnedBuffer};
pub(super) struct Scopes {
    pub(super) groups: Vec<pool::Group>,
    _charge: Option<Lease>,
}
impl Scopes {
    pub(super) fn new() -> Result<Box<Self>, Error> {
        Ok(Box::new(Self {
            groups: Vec::new(),
            _charge: crate::runtime::parallel::reserve_current(std::mem::size_of::<Self>())?,
        }))
    }
}
struct Message {
    values: Vec<(Type, u64, Option<OwnedBuffer>)>,
    _charge: Option<Lease>,
}
impl Message {
    fn new() -> Result<Self, Error> {
        Ok(Self {
            values: Vec::new(),
            _charge: crate::runtime::parallel::reserve_current(128)?,
        })
    }
    fn install(
        mut self,
        arena: &crate::runtime::arena::Arena,
        cleanups: &mut crate::runtime::cleanup::Cleanups,
        keys: &[usize],
    ) -> Result<Vec<Value>, Error> {
        let mut values = Vec::new();
        for ((ty, bits, owner), key) in self.values.iter_mut().zip(keys) {
            if let Some(owner) = owner {
                let raw = owner.take(arena)?;
                *bits = raw as u64;
                cleanups.insert(
                    *key,
                    crate::runtime::cleanup::Entry {
                        keep_alive: None,
                        storage_size: 0,
                        storage: None,
                        owners: vec![Some(raw)],
                        hooks: vec![],
                    },
                );
            }
            values.push(Value::from_instance_bits(ty, *bits)?);
        }
        Ok(values)
    }
}
impl Execution {
    fn remote_factory(
        &mut self,
        module: &BytecodeModule,
        target: usize,
        args: &[Option<usize>],
    ) -> Result<pool::Factory, Error> {
        let code = pool::code::<crate::vm::parallel::Code>()
            .ok_or_else(|| abi_error("Missing frozen interpreter code"))?;
        let crate::vm::parallel::Code::Interpreted(frozen) = code.as_ref() else {
            return Err(abi_error("Wrong frozen backend"));
        };
        let frozen = frozen.clone();
        let sig = match &module.callees[target] {
            Callee::User(f) => &f.info.signature,
            Callee::Host(h) => h.signature(),
            _ => return Err(abi_error("Unavailable parallel callee")),
        };
        let mut packet = Message::new()?;
        let mut index = 0;
        for ty in &sig.params {
            let source = args[index];
            let bits = source.map_or(Ok(0), |src| {
                self.frame_mut(self.current).slots[src].instance_bits()
            })?;
            let owner = if *ty == Type::String {
                let owner = std::ptr::NonNull::new(bits as *mut crate::runtime::buffer::Buffer)
                    .ok_or_else(|| abi_error("Invalid text capture"))?;
                let owner = unsafe { OwnedBuffer::new(&self.arena, owner) }?;
                self.frame_mut(self.current).cleanups.take(source.unwrap());
                Some(owner)
            } else {
                None
            };
            packet.values.push((ty.clone(), bits, owner));
            index += ty.parameter_slots();
        }
        let limits = self.limits;
        Ok(Box::new(move || {
            let module = frozen.instantiate()?;
            match &module.callees[target] {
                Callee::User(function) => {
                    let args = vec![Value::Unit; function.info.parameter_slots()];
                    let mut execution =
                        Execution::new(Frame::new(function.clone(), args.iter())?, limits, true);
                    let keys: Vec<_> = (0..args.len()).collect();
                    let mut cleanups = Default::default();
                    let values = packet.install(&execution.arena, &mut cleanups, &keys)?;
                    let frame = execution.frame_mut(0);
                    frame.slots[..values.len()].clone_from_slice(&values);
                    frame.cleanups = cleanups;
                    execution.parallel_child = true;
                    let ret = function.info.signature.ret.clone();
                    Ok(Box::new(Worker {
                        execution,
                        module,
                        ret,
                    }) as Box<dyn pool::Job>)
                }
                Callee::Host(host) => {
                    let arena = crate::runtime::arena::Arena::default();
                    let keys: Vec<_> = (0..packet.values.len()).collect();
                    let values = packet.install(&arena, &mut Default::default(), &keys)?;
                    let pending = HostPending::new(host.clone(), values.iter())?;
                    Ok(Box::new(HostWorker {
                        pending,
                        arena,
                        ret: host.signature().ret.clone(),
                    }) as Box<dyn pool::Job>)
                }
                _ => Err(abi_error("Unavailable parallel callee")),
            }
        }))
    }
    pub(super) fn remote_concurrent(&mut self, module: &BytecodeModule) -> Result<(), Error> {
        let frame = self.frame_mut(self.current);
        let Instruction::Concurrent { kind, branches, .. } = &frame.function.code[frame.pc] else {
            unreachable!()
        };
        let kind = if *kind == ConcurrentKind::Join { 0 } else { 1 };
        let branches: Vec<_> = branches
            .iter()
            .map(|b| (b.target.unwrap(), b.args.clone(), b.done, b.result))
            .collect();
        let mut factories = Vec::new();
        for (target, args, _, _) in &branches {
            factories.push(self.remote_factory(module, *target, args)?);
        }
        let mut group = pool::Group::new(kind);
        group.publish(factories)?;
        self.frame_mut(self.current).pending = Some(Pending::Parallel {
            group: Box::new(group),
            branches: branches
                .into_iter()
                .map(|(_, _, done, result)| (done, result))
                .collect(),
            delivered: false,
        });
        Ok(())
    }
    pub(super) fn remote_branch(&mut self, module: &BytecodeModule) -> Result<(), Error> {
        let frame = self.frame_mut(self.current);
        let Instruction::TaskBranch { target, args } = &frame.function.code[frame.pc] else {
            unreachable!()
        };
        let (target, args) = (*target, args.clone());
        let factory = self.remote_factory(module, target, &args)?;
        let frame = self.frame_mut(self.current);
        frame
            .remote_scopes
            .as_mut()
            .unwrap()
            .groups
            .last_mut()
            .unwrap()
            .publish(vec![factory])?;
        frame.pc += 1;
        Ok(())
    }
    pub(super) fn poll_remote(&mut self) -> Result<bool, Error> {
        let id = self.current;
        let frame = self.frame_mut(id);
        let Some(Pending::Parallel { group, .. }) = &mut frame.pending else {
            unreachable!()
        };
        if !group.poll()? {
            return Ok(false);
        }
        let Some(Pending::Parallel {
            mut group,
            branches,
            ..
        }) = frame.pending.take()
        else {
            unreachable!()
        };
        while let Some(index) = group.next_result() {
            let message = group
                .take(index)?
                .downcast::<Message>()
                .map_err(|_| abi_error("Invalid interpreted packet"))?;
            let (done, result) = branches[index];
            if let Some(slot) = result {
                let mut entries = Default::default();
                let values = message.install(&self.arena, &mut entries, &[slot])?;
                let frame = self.frame_mut(id);
                frame.slots[slot] = values.into_iter().next().unwrap();
                if let Some(entry) = entries.take(slot) {
                    frame.cleanups.insert(slot, entry);
                }
            }
            self.frame_mut(id).slots[done] = Value::Bool(true);
        }
        let frame = self.frame_mut(id);
        frame.pending = Some(Pending::Parallel {
            group,
            branches,
            delivered: true,
        });
        frame.pc += 1;
        Ok(true)
    }
}
struct Worker {
    execution: Execution,
    module: Rc<BytecodeModule>,
    ret: Type,
}
impl pool::Job for Worker {
    fn poll(&mut self) -> std::task::Poll<Result<pool::Output, Error>> {
        match self.execution.poll(&self.module) {
            Poll::Pending => std::task::Poll::Pending,
            Poll::Failed(e) => std::task::Poll::Ready(Err(e)),
            Poll::Complete(value) => std::task::Poll::Ready((|| {
                let mut result = Message::new()?;
                result.values.push((
                    self.ret.clone(),
                    value.instance_bits()?,
                    self.execution.parallel_output.take(),
                ));
                Ok(Box::new(result) as pool::Output)
            })()),
        }
    }
}
struct HostWorker {
    pending: HostPending,
    arena: crate::runtime::arena::Arena,
    ret: Type,
}
impl pool::Job for HostWorker {
    fn poll(&mut self) -> std::task::Poll<Result<pool::Output, Error>> {
        match self.pending.poll(&mut self.arena) {
            Ok(None) => std::task::Poll::Pending,
            Err(e) => std::task::Poll::Ready(Err(e)),
            Ok(Some(value)) => std::task::Poll::Ready((|| {
                self.pending.finish(&mut self.arena)?;
                let mut result = Message::new()?;
                result
                    .values
                    .push((self.ret.clone(), value.instance_bits()?, None));
                Ok(Box::new(result) as pool::Output)
            })()),
        }
    }
}
impl Drop for HostWorker {
    fn drop(&mut self) {
        let _ = self.pending.finish(&mut self.arena);
    }
}
