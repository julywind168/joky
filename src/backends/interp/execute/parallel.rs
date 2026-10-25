//! Transfer checked flat packets and exclusive text; construct activations locally.
use super::*;
use crate::runtime::parallel::{
    packet::{Packet, Shape},
    pool, Lease,
};
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
fn install(
    packet: Packet,
    ty: &Type,
    arena: &mut crate::runtime::arena::Arena,
    cleanups: &mut crate::runtime::cleanup::Cleanups,
    base: usize,
) -> Result<Vec<Value>, Error> {
    let words = packet.install(arena, cleanups, base, None)?;
    ty.value_lanes()
        .iter()
        .zip(words)
        .map(|(ty, bits)| Value::from_instance_bits(ty, bits))
        .collect()
}
fn start_worker(
    module: Rc<BytecodeModule>,
    target: usize,
    packets: Vec<Packet>,
    limits: InterpreterLimits,
) -> Result<Box<dyn pool::Job>, Error> {
    match &module.callees[target] {
        Callee::User(function) => start_function(module.clone(), function.clone(), packets, limits),
        Callee::Host(host) => {
            let mut arena = crate::runtime::arena::Arena::default();
            let mut values = Vec::new();
            for (packet, ty) in packets.into_iter().zip(&host.signature().params) {
                let mut value = install(
                    packet,
                    ty,
                    &mut arena,
                    &mut Default::default(),
                    values.len(),
                )?;
                if *ty == Type::Unit {
                    value.push(Value::Unit);
                }
                values.extend(value);
            }
            let pending = HostPending::new(host.clone(), values.iter())?;
            Ok(Box::new(HostWorker {
                pending,
                arena,
                ret: host.signature().ret.clone(),
            }))
        }
        _ => Err(abi_error("Unavailable parallel callee")),
    }
}
fn start_function(
    module: Rc<BytecodeModule>,
    function: Arc<BytecodeFunction>,
    packets: Vec<Packet>,
    limits: InterpreterLimits,
) -> Result<Box<dyn pool::Job>, Error> {
    let shape = Shape::of(&function.info.signature.ret)?;
    let shape_charge =
        crate::runtime::parallel::reserve_current(std::mem::size_of::<Worker>() + shape.bytes())?;
    let args = vec![Value::Unit; function.info.parameter_slots()];
    let mut execution = Execution::new(Frame::new(function.clone(), args.iter())?, limits, true);
    let mut base = 0;
    for (packet, ty) in packets.into_iter().zip(&function.info.signature.params) {
        // Install directly in the activation so partial installation is cleaned
        // if a later packet conversion fails.
        let frame = execution.nodes[0].as_mut().unwrap();
        let values = install(
            packet,
            ty,
            &mut execution.arena,
            frame.activation.cleanups_mut(),
            base,
        )?;
        execution.frame_mut(0).slots[base..base + values.len()].clone_from_slice(&values);
        base += ty.parameter_slots();
    }
    execution.parallel_child = Some(shape);
    if function.info.signature.ret.is_aggregate() {
        execution.root_aggregate = Some(Vec::new());
    }
    Ok(Box::new(Worker {
        execution,
        module,
        _shape_charge: shape_charge,
    }))
}
pub(crate) fn start_root(
    code: &FrozenEntry,
    name: &str,
    packets: Vec<Packet>,
    limits: InterpreterLimits,
) -> Result<Box<dyn pool::Job>, Error> {
    let module = code.instantiate()?;
    let function = module
        .functions
        .get(name)
        .cloned()
        .ok_or_else(|| abi_error("Unknown parallel entry"))?;
    start_function(module, function, packets, limits)
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
        let mut packets = Vec::new();
        let mut base = 0;
        for ty in &sig.params {
            let shape = Shape::of(ty)?;
            let words = args[base..base + shape.lanes()]
                .iter()
                .map(|source| {
                    source.map_or(Ok(0), |src| {
                        self.frame_mut(self.current).slots[src].instance_bits()
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let entry = shape
                .owners()
                .first()
                .and_then(|key| args[base + key])
                .and_then(|key| self.frame_mut(self.current).cleanups.take(key));
            packets.push(unsafe { Packet::capture(shape, words, entry, &mut self.arena)? });
            base += ty.parameter_slots();
        }
        let limits = self.limits;
        Ok(Box::new(move || {
            start_worker(frozen.instantiate()?, target, packets, limits)
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
    pub(super) fn poll_remote(&mut self, module: &BytecodeModule) -> Result<bool, Error> {
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
                .downcast::<Packet>()
                .map_err(|_| abi_error("Invalid interpreted packet"))?;
            let (done, result) = branches[index];
            let (target, destinations) = {
                let frame = self.frame_mut(id);
                let Instruction::Concurrent { branches, .. } = &frame.function.code[frame.pc]
                else {
                    unreachable!()
                };
                let branch = &branches[index];
                (
                    branch.target.unwrap(),
                    if branch.results.is_empty() {
                        result.into_iter().collect()
                    } else {
                        branch.results.clone()
                    },
                )
            };
            let ty = match &module.callees[target] {
                Callee::User(f) => &f.info.signature.ret,
                Callee::Host(h) => &h.signature().ret,
                _ => return Err(abi_error("Unavailable parallel result")),
            };
            let key = message
                .shape
                .owners()
                .first()
                .map(|&lane| destinations[lane]);
            let frame = self.nodes[id].as_mut().unwrap();
            let words = message.install(
                &mut self.arena,
                frame.activation.cleanups_mut(),
                destinations.first().copied().unwrap_or(0),
                key,
            )?;
            let values = ty
                .value_lanes()
                .iter()
                .zip(words)
                .map(|(ty, bits)| Value::from_instance_bits(ty, bits))
                .collect::<Result<Vec<_>, _>>()?;
            let frame = self.frame_mut(id);
            for (slot, value) in destinations.into_iter().zip(values) {
                frame.slots[slot] = value;
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
    _shape_charge: Option<Lease>,
}
impl pool::Job for Worker {
    fn poll(&mut self) -> std::task::Poll<Result<pool::Output, Error>> {
        match self.execution.poll(&self.module) {
            Poll::Pending => std::task::Poll::Pending,
            Poll::Failed(e) => std::task::Poll::Ready(Err(e)),
            Poll::Complete(_) => std::task::Poll::Ready(
                self.execution
                    .parallel_output
                    .take()
                    .map(|packet| Box::new(packet) as pool::Output)
                    .ok_or_else(|| abi_error("Missing interpreted packet")),
            ),
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
                let shape = Shape::of(&self.ret)?;
                let words = if shape.lanes() == 0 {
                    vec![]
                } else {
                    vec![value.instance_bits()?]
                };
                let result = unsafe { Packet::capture(shape, words, None, &mut self.arena)? };
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
