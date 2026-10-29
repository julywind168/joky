//! Transfer checked flat packets and exclusive text; construct activations locally.
use super::*;
use crate::runtime::parallel::{
    packet::{Packet, Shape},
    pool,
    scoped::{Input, InputShape, Scope},
    Lease,
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
    packet: Input,
    ty: &Type,
    arena: &mut crate::runtime::arena::Arena,
    cleanups: &mut crate::runtime::cleanup::Cleanups,
    base: usize,
    loans: &mut Vec<Arc<Scope>>,
) -> Result<Vec<Value>, Error> {
    let words = packet.install(arena, cleanups, base, loans)?;
    ty.value_lanes()
        .iter()
        .zip(words)
        .map(|(ty, bits)| Value::from_instance_bits(ty, bits))
        .collect()
}
fn start_worker(
    module: Rc<BytecodeModule>,
    target: usize,
    packets: Vec<Input>,
    limits: InterpreterLimits,
) -> Result<Box<dyn pool::Job>, Error> {
    match &module.callees[target] {
        Callee::User(function) => start_function(module.clone(), function.clone(), packets, limits),
        Callee::Host(host) => {
            let mut arena = crate::runtime::arena::Arena::default();
            let mut values = Vec::new();
            let mut loans = Vec::new();
            for (packet, ty) in packets.into_iter().zip(&host.signature().params) {
                let mut value = install(
                    packet,
                    ty,
                    &mut arena,
                    &mut Default::default(),
                    values.len(),
                    &mut loans,
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
                _loans: loans,
            }))
        }
        _ => Err(abi_error("Unavailable parallel callee")),
    }
}
fn start_function(
    module: Rc<BytecodeModule>,
    function: Arc<BytecodeFunction>,
    packets: Vec<Input>,
    limits: InterpreterLimits,
) -> Result<Box<dyn pool::Job>, Error> {
    let shape = Shape::of(&function.info.signature.ret)?;
    let shape_charge = crate::runtime::parallel::reserve_current(
        std::mem::size_of::<Worker>()
            + shape.bytes()
            + packets.len() * std::mem::size_of::<Arc<Scope>>(),
    )?;
    let args = vec![Value::Unit; function.info.parameter_slots()];
    let mut execution = Execution::new(Frame::new(function.clone(), args.iter())?, limits, true);
    let mut base = 0;
    let mut loans = Vec::new();
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
            &mut loans,
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
        _loans: loans,
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
    start_function(
        module,
        function,
        packets.into_iter().map(Input::owned).collect(),
        limits,
    )
}
impl Execution {
    pub(super) fn scoped_concurrent(&self, module: &BytecodeModule) -> bool {
        if !pool::active() {
            return false;
        }
        let Activation::User(frame) = &self.node(self.current).activation else {
            return false;
        };
        let Instruction::Concurrent {
            kind: ConcurrentKind::Join | ConcurrentKind::Race,
            branches,
            ..
        } = &frame.function.code[frame.pc]
        else {
            return false;
        };
        branches.iter().all(|branch| {
            if branch.indirect.is_some() {
                return false;
            }
            let Some(Callee::User(function)) = branch.target.and_then(|id| module.callees.get(id))
            else {
                return false;
            };
            if !function.scoped || Shape::of(&function.info.signature.ret).is_err() {
                return false;
            }
            let mut base = 0;
            function.info.signature.params.iter().all(|ty| {
                let Ok(input) = InputShape::of(ty) else {
                    return false;
                };
                let eligible = match input {
                    InputShape::Owned(_) => true,
                    InputShape::Borrowed(shape) => branch.args[base]
                        .and_then(|key| frame.slots[key].instance_bits().ok())
                        .is_some_and(|word| unsafe { shape.eligible(word) }),
                };
                base += ty.parameter_slots();
                eligible
            })
        })
    }

    fn remote_factory(
        &mut self,
        module: &BytecodeModule,
        target: usize,
        args: &[Option<usize>],
        scope: Option<Arc<Scope>>,
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
            let input = if scope.is_some() {
                InputShape::of(ty)?
            } else {
                InputShape::Owned(Shape::of(ty)?)
            };
            match input {
                InputShape::Borrowed(shape) => {
                    let key = args[base].ok_or_else(|| abi_error("Missing scoped input"))?;
                    let word = self.frame_mut(self.current).slots[key].instance_bits()?;
                    packets.push(unsafe {
                        Input::borrow(shape, word, scope.as_ref().unwrap().clone())?
                    });
                }
                InputShape::Owned(shape) => {
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
                    packets.push(Input::owned(unsafe {
                        Packet::capture(shape, words, entry, &mut self.arena)?
                    }));
                }
            }
            base += ty.parameter_slots();
        }
        let limits = self.limits;
        Ok(Box::new(move || {
            start_worker(frozen.instantiate()?, target, packets, limits)
        }))
    }
    pub(super) fn remote_concurrent(
        &mut self,
        module: &BytecodeModule,
        scoped: bool,
    ) -> Result<(), Error> {
        let frame = self.frame_mut(self.current);
        let Instruction::Concurrent { kind, branches, .. } = &frame.function.code[frame.pc] else {
            unreachable!()
        };
        let kind = if *kind == ConcurrentKind::Join { 0 } else { 1 };
        let branches: Vec<_> = branches
            .iter()
            .map(|b| {
                let target = match b.indirect {
                    Some(slot) => match frame.slots[slot] {
                        Value::Function(target) if target < module.callees.len() => target,
                        _ => return Err(abi_error("Invalid parallel function value")),
                    },
                    None => b.target.unwrap(),
                };
                Ok((target, b.args.clone(), b.done, b.result))
            })
            .collect::<Result<_, Error>>()?;
        let scope = if scoped {
            Some(Scope::new(crate::runtime::task_scope::fresh_identity()?)?)
        } else {
            None
        };
        let mut group = pool::Group::new(kind);
        let _capture = group.capture_guard();
        let publication = (|| {
            let mut factories = Vec::new();
            for (target, args, _, _) in &branches {
                factories.push(self.remote_factory(module, *target, args, scope.clone())?);
            }
            group.publish(factories)
        })();
        self.frame_mut(self.current).pending = Some(Pending::Parallel {
            group: Box::new(group),
            branches: branches
                .into_iter()
                .map(|(target, _, done, result)| (target, done, result))
                .collect(),
            delivered: false,
        });
        publication
    }
    pub(super) fn remote_branch(&mut self, module: &BytecodeModule) -> Result<(), Error> {
        let frame = self.frame_mut(self.current);
        let Instruction::TaskBranch { target, args } = &frame.function.code[frame.pc] else {
            unreachable!()
        };
        let (target, args) = (*target, args.clone());
        let factory = self.remote_factory(module, target, &args, None)?;
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
            let (target, done, result) = branches[index];
            let destinations = {
                let frame = self.frame_mut(id);
                let Instruction::Concurrent { branches, .. } = &frame.function.code[frame.pc]
                else {
                    unreachable!()
                };
                let branch = &branches[index];
                if branch.results.is_empty() {
                    result.into_iter().collect()
                } else {
                    branch.results.clone()
                }
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
    _loans: Vec<Arc<Scope>>,
}
impl pool::Job for Worker {
    fn poll(&mut self) -> std::task::Poll<Result<pool::Output, Error>> {
        match self.execution.poll(&self.module) {
            Poll::Pending => {
                let mut plan = crate::runtime::driver::WaitPlan::default();
                for node in self.execution.nodes.iter().flatten() {
                    if let Activation::Host(host) = &node.activation {
                        plan.merge(host.wait_plan());
                    }
                }
                if self.execution.cooperative_yield {
                    plan.add(Some(crate::runtime::driver::Wake::Immediate));
                }
                pool::wait_plan(plan);
                std::task::Poll::Pending
            }
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
    _loans: Vec<Arc<Scope>>,
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
