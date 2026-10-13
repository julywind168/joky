//! Iterative execution: language calls and scopes never recurse on the Rust stack.
use super::{host::HostPending, *};
use crate::{
    core::integer::IntegerOutcome,
    runtime::suspension::{InterpretedTask, ResumeError},
    vm::InterpreterLimits,
    RuntimeFrame,
};

/// Truncate an already range-checked float into any integer target. The
/// saturating casts cannot fire because the standard library proved the
/// rounded value inside the target's exact power-of-two bounds.
macro_rules! trunc_float {
    ($value:expr, $target:expr) => {
        match $target.integer() {
            Some(IntegerType::Int8) => Value::Int8($value as i8),
            Some(IntegerType::Int16) => Value::Int16($value as i16),
            Some(IntegerType::Int32) => Value::Int32($value as i32),
            Some(IntegerType::Int64) => Value::Int64($value as i64),
            Some(IntegerType::UInt8) => Value::UInt8($value as u8),
            Some(IntegerType::UInt16) => Value::UInt16($value as u16),
            Some(IntegerType::UInt32) => Value::UInt32($value as u32),
            Some(IntegerType::UInt64) => Value::UInt64($value as u64),
            _ => unreachable!("float conversion targets are always numeric"),
        }
    };
}

pub(crate) fn call_sync(
    module: &BytecodeModule,
    name: &str,
    args: &[Value],
    limits: InterpreterLimits,
) -> Result<Value, Error> {
    let function = lookup(module, name)?;
    if function.info.suspends {
        return Err(runtime_error(
            ErrorCode::InvalidCall,
            format!("Function `{name}` requires call_async"),
        ));
    }
    let mut execution = Execution::new(Frame::new(function, args.iter())?, limits, false);
    execution.initialize_text_inputs()?;
    match execution.poll(module) {
        Poll::Complete(value) => Ok(value),
        Poll::Failed(error) => Err(error),
        Poll::Pending => Err(runtime_error(
            ErrorCode::InvalidState,
            "Synchronous function suspended",
        )),
    }
}

/// Checked compiler-only root lane API. The VM retains only checked owning root lanes.
pub(crate) fn call_instance(
    module: &BytecodeModule,
    name: &str,
    bits: &[u64],
    limits: InterpreterLimits,
    arena: &mut crate::runtime::arena::Arena,
) -> Result<Vec<u64>, Error> {
    let function = module
        .functions
        .get(name)
        .cloned()
        .ok_or_else(|| abi_error("Unknown instance entry"))?;
    if !(crate::core::instance::eligible(&function.info)
        || function.info.domain_entry
            && (crate::core::state_domain::pair_eligible(&function.info)
                || crate::core::state_domain::migration_eligible(&function.info)))
        || bits.len() != function.info.signature.params.len()
    {
        return Err(abi_error("Invalid persistent instance entry"));
    }
    let ret = function.info.signature.ret.clone();
    let args = function
        .info
        .signature
        .params
        .iter()
        .zip(bits)
        .map(|(ty, &bits)| Value::from_instance_bits(ty, bits))
        .collect::<Result<Vec<_>, _>>()?;
    let mut execution = Execution::new(Frame::new(function, args.iter())?, limits, false);
    execution.cleanup_mode = true;
    execution.arena = std::mem::take(arena);
    if ret.is_aggregate() {
        execution.root_aggregate = Some(Vec::new());
    }
    let result = match execution.poll(module) {
        Poll::Complete(value) => execution
            .root_aggregate
            .take()
            .unwrap_or_else(|| vec![value])
            .iter()
            .map(Value::instance_bits)
            .collect(),
        Poll::Failed(error) => Err(error),
        Poll::Pending => Err(abi_error("Persistent instance entry suspended")),
    };
    execution.release();
    *arena = std::mem::take(&mut execution.arena);
    result
}

pub(crate) fn start(
    module: Rc<BytecodeModule>,
    name: &str,
    args: &[Value],
    integer: &crate::Type,
    limits: InterpreterLimits,
) -> Result<Box<dyn InterpretedTask>, Error> {
    let function = lookup(&module, name)?;
    if !function.info.suspends {
        return Err(runtime_error(
            ErrorCode::InvalidCall,
            format!("Function `{name}` is not marked @suspends"),
        ));
    }
    if !function
        .info
        .signature
        .accepts_integer_async(args.len(), integer)
    {
        return Err(runtime_error(
            ErrorCode::InvalidCall,
            format!("Async call signature mismatch for `{name}`"),
        ));
    }
    let frame = Frame::new(function, args.iter())?;
    Ok(Box::new(Task {
        module,
        execution: Execution::new(frame, limits, true),
        result: None,
        text_result: None,
    }))
}

/// Prepare a checked scalar root without executing its first instruction.
pub(crate) fn prepare_scalar(
    module: Rc<BytecodeModule>,
    name: &str,
    args: &[crate::vm::InstanceValue],
    limits: InterpreterLimits,
) -> Result<Box<dyn InterpretedTask>, Error> {
    let function = module
        .functions
        .get(name)
        .cloned()
        .ok_or_else(|| abi_error("Unknown scalar request entry"))?;
    let signature = &function.info.signature;
    if !function.info.suspends
        || !crate::core::instance::scalar(&signature.ret)
        || signature.params != args.iter().map(|v| v.value_type()).collect::<Vec<_>>()
    {
        return Err(abi_error("Async scalar call signature mismatch"));
    }
    let values = args
        .iter()
        .map(|v| Value::from_instance_bits(&v.value_type(), v.bits()))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Box::new(Task {
        module,
        execution: Execution::new(Frame::new(function, values.iter())?, limits, true),
        result: None,
        text_result: None,
    }))
}

/// The caller has checked both flat schemas and every scalar input lane.
pub(crate) fn prepare_script(
    module: Rc<BytecodeModule>,
    name: &str,
    bits: &[u64],
    limits: InterpreterLimits,
) -> Result<Box<dyn InterpretedTask>, Error> {
    let function = module
        .functions
        .get(name)
        .cloned()
        .ok_or_else(|| abi_error("Unknown Script entry"))?;
    let input = &function.info.signature.params[0];
    let args = if *input == Type::Unit {
        vec![Value::Unit]
    } else {
        input
            .scalar_types()
            .iter()
            .zip(bits)
            .map(|(ty, &bits)| Value::from_instance_bits(ty, bits))
            .collect::<Result<Vec<_>, _>>()?
    };
    let aggregate =
        function.info.signature.ret.is_aggregate() || function.info.signature.ret == Type::Unit;
    let mut execution = Execution::new(Frame::new(function, args.iter())?, limits, true);
    if aggregate {
        execution.root_aggregate = Some(Vec::new());
    }
    Ok(Box::new(Task {
        module,
        execution,
        result: None,
        text_result: None,
    }))
}

pub(crate) fn start_text(
    module: Rc<BytecodeModule>,
    name: &str,
    input: Option<&str>,
    limits: InterpreterLimits,
) -> Result<Box<dyn InterpretedTask>, Error> {
    let function = lookup(&module, name)?;
    let params = if input.is_some() {
        vec![crate::Type::String]
    } else {
        vec![]
    };
    if !function.info.suspends
        || function.info.signature != FunctionSignature::new(params, crate::Type::String)
    {
        return Err(abi_error("Async text call signature mismatch"));
    }
    let args: Vec<_> = input
        .into_iter()
        .map(|text| Value::String(Rc::from(text)))
        .collect();
    let mut execution = Execution::new(Frame::new(function, args.iter())?, limits, true);
    execution.initialize_text_inputs()?;
    Ok(Box::new(Task {
        module,
        execution,
        result: None,
        text_result: None,
    }))
}

fn lookup(module: &BytecodeModule, name: &str) -> Result<Rc<BytecodeFunction>, Error> {
    module
        .functions
        .get(name)
        .filter(|function| !function.info.internal)
        .cloned()
        .ok_or_else(|| runtime_error(ErrorCode::NotFound, format!("Function `{name}` not found")))
}

pub(super) struct Frame {
    cleanups: crate::runtime::cleanup::Cleanups,
    cleanup_locals: memory::Locals,
    locals: memory::Locals,
    function: Rc<BytecodeFunction>,
    slots: Vec<Value>,
    pc: usize,
    pending: Option<Pending>,
    task_scopes: Vec<TaskScopeState>,
}

impl Frame {
    pub(super) fn new<'a>(
        function: Rc<BytecodeFunction>,
        args: impl ExactSizeIterator<Item = &'a Value>,
    ) -> Result<Self, Error> {
        if args.len() != function.info.parameter_slots() || function.info.frame_slots < args.len() {
            return Err(runtime_error(
                ErrorCode::InvalidState,
                "Interpreter frame argument layout mismatch",
            ));
        }
        let mut slots = vec![Value::Unit; function.info.frame_slots];
        for (slot, value) in slots.iter_mut().zip(args) {
            *slot = value.clone();
        }
        Ok(Self {
            cleanups: Default::default(),
            cleanup_locals: Default::default(),
            locals: Default::default(),
            function,
            slots,
            pc: 0,
            pending: None,
            task_scopes: Vec::new(),
        })
    }

    fn trace(&self) -> RuntimeFrame {
        RuntimeFrame {
            function: self.function.name.clone(),
            pc: self.pc,
            span: self
                .function
                .spans
                .get(self.pc)
                .copied()
                .unwrap_or(self.function.info.span),
        }
    }
}

// Children are indices into a flat arena, including scope branches. Both
// execution and cancellation/drop remain iterative at arbitrary language depth.
enum Pending {
    Call {
        child: usize,
        result: Option<usize>,
    },
    Scope {
        kind: ScopeKind,
        branches: Vec<Branch>,
        next: usize,
        pending: bool,
    },
    /// Joining the innermost open dynamic task scope; the record stays on
    /// the frame's scope stack until every registered child has finished.
    TaskJoin,
}

struct Branch {
    child: Option<usize>,
    done_slot: usize,
}

/// One open dynamic task scope on a frame: registered children in creation
/// order (`None` once finished), the polling cursor, and whether the current
/// round observed a Pending child.
#[derive(Default)]
struct TaskScopeState {
    children: Vec<Option<usize>>,
    cursor: usize,
    pending: bool,
}

impl TaskScopeState {
    fn compact(&mut self) {
        self.cursor = self.children[..self.cursor].iter().flatten().count();
        self.children.retain(Option::is_some);
        if self.children.capacity() > self.children.len().saturating_mul(4).max(16) {
            self.children.shrink_to(self.children.len().max(8));
        }
    }
}

enum Activation {
    User(Frame),
    Host(HostPending),
}

struct Node {
    parent: Option<usize>,
    activation: Activation,
}

pub(super) enum Poll {
    Pending,
    Complete(Value),
    Failed(Error),
}

enum Step {
    Call {
        target: usize,
        result: Option<usize>,
    },
    Scope,
    TaskBranch,
    TaskJoin,
    Complete(Value),
    CompleteAggregate,
    Yield,
    Failed(Error),
}
enum Charge {
    Ready,
    Yield,
    Exhausted,
}

struct Budget {
    remaining: Option<u64>,
    slice: Option<u64>,
}

impl Budget {
    #[inline]
    fn charge(&mut self) -> Charge {
        if self.remaining == Some(0) {
            return Charge::Exhausted;
        }
        if self.slice == Some(0) {
            return Charge::Yield;
        }
        if let Some(remaining) = &mut self.remaining {
            *remaining -= 1;
        }
        if let Some(slice) = &mut self.slice {
            *slice -= 1;
        }
        Charge::Ready
    }
}

pub(super) struct Execution {
    root_aggregate: Option<Vec<Value>>,
    cleanup_mode: bool,
    cleanup_module: Option<Rc<BytecodeModule>>,
    nodes: Vec<Option<Node>>,
    free: Vec<usize>,
    live: usize,
    current: usize,
    limits: InterpreterLimits,
    budget: Budget,
    asynchronous: bool,
    cooperative_yield: bool,
    round_child: Option<usize>,
    arena: crate::runtime::arena::Arena,
}

impl Execution {
    pub(super) fn new(frame: Frame, limits: InterpreterLimits, asynchronous: bool) -> Self {
        Self {
            cleanup_mode: false,
            root_aggregate: None,
            cleanup_module: None,
            nodes: vec![Some(Node {
                parent: None,
                activation: Activation::User(frame),
            })],
            free: Vec::new(),
            live: 1,
            current: 0,
            limits,
            budget: Budget {
                remaining: limits.instruction_budget,
                slice: None,
            },
            asynchronous,
            cooperative_yield: false,
            round_child: None,
            arena: crate::runtime::arena::Arena::default(),
        }
    }

    fn node(&self, id: usize) -> &Node {
        self.nodes[id].as_ref().expect("live activation")
    }
    fn node_mut(&mut self, id: usize) -> &mut Node {
        self.nodes[id].as_mut().expect("live activation")
    }
    fn frame_mut(&mut self, id: usize) -> &mut Frame {
        let Activation::User(frame) = &mut self.node_mut(id).activation else {
            unreachable!("user parent")
        };
        frame
    }

    fn insert(&mut self, activation: Activation, parent: usize) -> usize {
        let node = Some(Node {
            parent: Some(parent),
            activation,
        });
        self.live += 1;
        if let Some(id) = self.free.pop() {
            self.nodes[id] = node;
            id
        } else {
            let id = self.nodes.len();
            self.nodes.push(node);
            id
        }
    }

    fn frame_capacity(&self, count: usize) -> Result<(), Error> {
        if count > self.limits.max_frames.saturating_sub(self.live) {
            Err(runtime_error(
                ErrorCode::ResourceLimit,
                "Interpreter frame limit exceeded",
            ))
        } else {
            Ok(())
        }
    }

    fn fail(&mut self, mut error: Error) -> Poll {
        let mut cursor = Some(self.current);
        while let Some(id) = cursor {
            let node = self.node(id);
            if let Activation::User(frame) = &node.activation {
                error.push_runtime_frame(frame.trace());
            }
            cursor = node.parent;
        }
        // Drop every sibling and local value immediately, even if a scheduler
        // retains the failed continuation to inspect its structured error.
        self.release();
        Poll::Failed(error)
    }

    fn release(&mut self) {
        if self.live != 0 {
            self.release_subtrees(vec![0]);
        }
        self.nodes = Vec::new();
        self.free = Vec::new();
        self.live = 0;
        if !self.cleanup_mode {
            self.arena = crate::runtime::arena::Arena::default();
        }
    }

    fn arguments<'a>(
        frame: &'a Frame,
        args: &'a [Option<usize>],
    ) -> impl ExactSizeIterator<Item = &'a Value> {
        args.iter()
            .map(|slot| slot.map_or(&Value::Unit, |slot| &frame.slots[slot]))
    }

    fn activation(
        module: &BytecodeModule,
        target: usize,
        frame: &Frame,
        args: &[Option<usize>],
    ) -> Result<Activation, Error> {
        match &module.callees[target] {
            Callee::User(function) => {
                Frame::new(function.clone(), Self::arguments(frame, args)).map(Activation::User)
            }
            Callee::Host(host) => {
                HostPending::new(host.clone(), Self::arguments(frame, args)).map(Activation::Host)
            }
        }
    }

    fn owned_arguments(
        module: &BytecodeModule,
        target: usize,
        args: &[Option<usize>],
    ) -> Vec<(usize, usize)> {
        let signature = match &module.callees[target] {
            Callee::User(function) => &function.info.signature,
            Callee::Host(host) => host.signature(),
        };
        let mut index = 0;
        let mut owned = Vec::new();
        for ty in &signature.params {
            if ty.is_owner() {
                let key = crate::core::ir::Slot {
                    index,
                    ty: ty.clone(),
                }
                .owner_key();
                owned.push((key, args[key].expect("owning argument")));
            }
            index += ty.parameter_slots();
        }
        owned
    }

    fn initialize_text_inputs(&mut self) -> Result<(), Error> {
        let Activation::User(frame) = &mut self.nodes[0].as_mut().unwrap().activation else {
            unreachable!()
        };
        let mut index = 0;
        for ty in &frame.function.info.signature.params {
            if *ty == crate::Type::String {
                let Value::String(text) = &frame.slots[index] else {
                    return Err(abi_error("A text root requires owned host text input"));
                };
                let owner =
                    crate::runtime::buffer::Buffer::copy_bytes(&mut self.arena, text.as_bytes())?;
                frame.slots[index] = Value::Reference(owner.as_ptr().cast());
                frame.cleanups.insert(
                    index,
                    crate::runtime::cleanup::Entry {
                        keep_alive: None,
                        storage_size: 0,
                        storage: None,
                        owners: vec![Some(owner.as_ptr())],
                        hooks: vec![],
                    },
                );
            }
            index += ty.parameter_slots();
        }
        Ok(())
    }

    fn start_call(
        &mut self,
        module: &BytecodeModule,
        target: usize,
        result: Option<usize>,
    ) -> Result<(), Error> {
        let id = self.current;
        let Activation::User(frame) = &self.nodes[id].as_ref().unwrap().activation else {
            unreachable!()
        };
        let args = match &frame.function.code[frame.pc] {
            Instruction::Call { args, .. } => args,
            Instruction::IndirectCall(call) => &call.args,
            Instruction::AggregateCall(call) => &call.args,
            _ => unreachable!(),
        };
        let callee = module.callees.get(target).ok_or_else(|| {
            runtime_error(ErrorCode::InvalidCall, "Invalid language function value")
        })?;
        if let Callee::Host(host) = callee {
            if !host.suspends() {
                let owned = Self::owned_arguments(module, target, args);
                let inputs: Vec<_> = Self::arguments(frame, args).cloned().collect();
                let entries: Vec<_> = owned
                    .into_iter()
                    .filter_map(|(_, key)| self.frame_mut(id).cleanups.take(key))
                    .collect();
                let mut value = host.call(inputs.iter(), &mut self.arena);
                for entry in entries {
                    if let Err(error) = unsafe { entry.run(&mut self.arena) } {
                        if value.is_ok() {
                            value = Err(error);
                        }
                    }
                }
                let value = value?;
                let frame = self.frame_mut(id);
                if let Some(slot) = result {
                    frame.slots[slot] = value;
                }
                frame.pc += 1;
                return Ok(());
            }
        }
        self.frame_capacity(1)?;
        let mut child = Self::activation(module, target, frame, args)?;
        let owned = Self::owned_arguments(module, target, args);
        // Preserve the caller's hooks until the callee successfully installs
        // its own snapshot. Registration itself may fail under allocation limits.
        let parent = self.frame_mut(id);
        for (dst, src) in owned {
            if let Some(entry) = parent.cleanups.take(src) {
                match &mut child {
                    Activation::User(child) => child.cleanups.insert(dst, entry),
                    Activation::Host(child) => child.cleanups.insert(dst, entry),
                }
            }
        }
        let child = self.insert(child, id);
        self.frame_mut(id).pending = Some(Pending::Call { child, result });
        self.current = child;
        Ok(())
    }

    /// Copy flattened results while both activations are alive. This keeps
    /// aggregates out of the scalar Value enum and needs no aggregate allocation.
    fn complete_aggregate(&mut self) -> Result<(), Error> {
        let child_id = self.current;
        if self.node(child_id).parent.is_none() && self.root_aggregate.is_some() {
            let frame = self.frame_mut(child_id);
            let result = frame
                .function
                .aggregate_result
                .iter()
                .map(|&slot| frame.slots[slot].clone())
                .collect();
            self.root_aggregate = Some(result);
            return Ok(());
        }
        let parent_id = self.node(child_id).parent.ok_or_else(|| {
            runtime_error(
                ErrorCode::InvalidCall,
                "Aggregate results require an internal language caller",
            )
        })?;
        if let Activation::Host(host) = &self.node(child_id).activation {
            let value = host
                .aggregate_result()
                .expect("validated resource byte result");
            let parent = self.frame_mut(parent_id);
            let results = match &parent.function.code[parent.pc] {
                Instruction::AggregateCall(call) => &call.results,
                Instruction::IndirectCall(call) => &call.results,
                _ => unreachable!("byte request result"),
            };
            debug_assert_eq!(results.len(), value.iter().flatten().count());
            for (&slot, value) in results.iter().zip(value.into_iter().flatten()) {
                parent.slots[slot] = value;
            }
            return Ok(());
        }
        let (child, parent) = if child_id < parent_id {
            let (left, right) = self.nodes.split_at_mut(parent_id);
            (&mut left[child_id], &mut right[0])
        } else {
            let (left, right) = self.nodes.split_at_mut(child_id);
            (&mut right[0], &mut left[parent_id])
        };
        let Activation::User(child) = &mut child.as_mut().unwrap().activation else {
            unreachable!()
        };
        let Activation::User(parent) = &mut parent.as_mut().unwrap().activation else {
            unreachable!()
        };
        let results = match &parent.function.code[parent.pc] {
            Instruction::AggregateCall(call) => &call.results,
            Instruction::IndirectCall(call) => &call.results,
            _ => unreachable!(),
        };
        debug_assert_eq!(results.len(), child.function.aggregate_result.len());
        for (&dst, &src) in results.iter().zip(&child.function.aggregate_result) {
            parent.slots[dst] = child.slots[src].clone();
        }
        if let Some(key) = child.function.owner_result {
            let lane = child
                .function
                .aggregate_result
                .iter()
                .position(|&slot| slot == key)
                .unwrap();
            if let Some(entry) = child.cleanups.take(key) {
                parent.cleanups.insert(results[lane], entry);
            }
        }
        Ok(())
    }

    fn start_scope(&mut self, module: &BytecodeModule) -> Result<(), Error> {
        let id = self.current;
        let Activation::User(frame) = &self.nodes[id].as_ref().unwrap().activation else {
            unreachable!()
        };
        let Instruction::Scope { kind, branches } = &frame.function.code[frame.pc] else {
            unreachable!()
        };
        let kind = *kind;
        self.frame_capacity(branches.len())?;
        // Construct all owned argument snapshots before polling any branch.
        let children: Vec<_> = branches
            .iter()
            .map(|branch| {
                let target = match branch.indirect {
                    Some(slot) => match frame.slots[slot] {
                        Value::Function(target) if target < module.callees.len() => target,
                        _ => {
                            return Err(runtime_error(
                                ErrorCode::InvalidCall,
                                "Invalid language function value",
                            ))
                        }
                    },
                    None => branch.target.expect("direct branch target"),
                };
                Self::activation(module, target, frame, &branch.args).map(|child| {
                    (
                        child,
                        branch.done,
                        Self::owned_arguments(module, target, &branch.args),
                    )
                })
            })
            .collect::<Result<_, _>>()?;
        let branches = children
            .into_iter()
            .map(|(mut child, done_slot, owned)| {
                for (dst, src) in owned {
                    if let Some(entry) = self.frame_mut(id).cleanups.take(src) {
                        match &mut child {
                            Activation::User(frame) => frame.cleanups.insert(dst, entry),
                            Activation::Host(host) => host.cleanups.insert(dst, entry),
                        }
                    }
                }
                Branch {
                    child: Some(self.insert(child, id)),
                    done_slot,
                }
            })
            .collect();
        self.frame_mut(id).pending = Some(Pending::Scope {
            kind,
            branches,
            next: 0,
            pending: false,
        });
        Ok(())
    }

    /// Drop just the losing subtrees, preserving ancestors, unrelated siblings,
    /// and the task arena. Walk indices so cancellation uses no Rust recursion.
    fn cancel_branches(&mut self, branches: Vec<Branch>) {
        self.release_subtrees(branches.into_iter().filter_map(|b| b.child).collect());
    }

    fn release_subtrees(&mut self, roots: Vec<usize>) {
        // Reused node indices do not encode nesting. Keep parents alive until
        // child callbacks have finished using their borrowed local storage.
        let mut pending: Vec<_> = roots.into_iter().map(|id| (id, false)).collect();
        while let Some((id, visited)) = pending.pop() {
            if !visited {
                pending.push((id, true));
                if let Activation::User(frame) = &self.node(id).activation {
                    match &frame.pending {
                        Some(Pending::Call { child, .. }) => pending.push((*child, false)),
                        Some(Pending::Scope { branches, .. }) => pending.extend(
                            branches
                                .iter()
                                .filter_map(|b| b.child)
                                .map(|id| (id, false)),
                        ),
                        Some(Pending::TaskJoin) | None => {}
                    }
                    for scope in &frame.task_scopes {
                        pending.extend(
                            scope
                                .children
                                .iter()
                                .flatten()
                                .copied()
                                .map(|id| (id, false)),
                        );
                    }
                }
                continue;
            }
            let mut node = self.nodes[id].take().expect("live cancelled branch");
            match &mut node.activation {
                Activation::User(frame) => {
                    let _ = unsafe { frame.cleanups.finish(&mut self.arena) };
                }
                Activation::Host(host) => {
                    let _ = host.finish(&mut self.arena);
                }
            }
            self.free.push(id);
            self.live -= 1;
        }
    }

    /// Select the next dynamic child to poll for this scheduling round:
    /// pre-order over frames, scopes outermost-first, children in registration
    /// order. Children already attempted this round (before their scope
    /// cursor) contribute only their subtree. A joining frame's top scope is
    /// driven by its join site; its children's subtrees still participate here.
    fn select_round_child(&self, root: usize) -> Option<(usize, usize, usize)> {
        enum Visit {
            Frame(usize),
            Child(usize, usize, usize),
        }
        let mut pending = vec![Visit::Frame(root)];
        while let Some(visit) = pending.pop() {
            let id = match visit {
                Visit::Child(frame, scope, slot) => return Some((frame, scope, slot)),
                Visit::Frame(id) => id,
            };
            let Activation::User(frame) = &self.node(id).activation else {
                continue;
            };
            match &frame.pending {
                Some(Pending::Call { child, .. }) => pending.push(Visit::Frame(*child)),
                Some(Pending::Scope { branches, .. }) => pending.extend(
                    branches
                        .iter()
                        .rev()
                        .filter_map(|branch| branch.child)
                        .map(Visit::Frame),
                ),
                Some(Pending::TaskJoin) | None => {}
            }
            let joining_top = matches!(frame.pending, Some(Pending::TaskJoin))
                .then(|| frame.task_scopes.len().saturating_sub(1));
            for (scope_index, scope) in frame.task_scopes.iter().enumerate().rev() {
                for (slot, child) in scope.children.iter().enumerate().rev() {
                    if let Some(child) = child {
                        if Some(scope_index) == joining_top || slot < scope.cursor {
                            pending.push(Visit::Frame(*child));
                        } else {
                            pending.push(Visit::Child(id, scope_index, slot));
                        }
                    }
                }
            }
        }
        None
    }

    /// Advance the scheduling round by one child, or close the round (reset
    /// the per-scope cursors) when every registered child was attempted.
    fn advance_task_round(&mut self) -> Option<usize> {
        self.round_child = match self.select_round_child(0) {
            Some((frame_id, scope_index, slot)) => {
                self.frame_mut(frame_id).task_scopes[scope_index].children[slot]
            }
            None => {
                self.reset_round_cursors();
                None
            }
        };
        self.round_child
    }

    fn reset_round_cursors(&mut self) {
        for node in self.nodes.iter_mut().flatten() {
            let Activation::User(frame) = &mut node.activation else {
                continue;
            };
            let joining_top = matches!(frame.pending, Some(Pending::TaskJoin))
                .then(|| frame.task_scopes.len().saturating_sub(1));
            for (scope_index, scope) in frame.task_scopes.iter_mut().enumerate() {
                scope.compact();
                if Some(scope_index) != joining_top {
                    scope.cursor = 0;
                    scope.pending = false;
                }
            }
        }
    }

    /// Record a polled child's outcome in the scope that registered it.
    /// Returns false for ordinary call or scope children.
    fn finish_task_child(frame: &mut Frame, child: usize, completed: bool) -> bool {
        for scope in frame.task_scopes.iter_mut().rev() {
            if let Some(slot) = scope
                .children
                .iter()
                .position(|entry| *entry == Some(child))
            {
                if completed {
                    scope.children[slot] = None;
                }
                if slot == scope.cursor {
                    scope.cursor += 1;
                }
                if !completed {
                    scope.pending = true;
                }
                return true;
            }
        }
        false
    }

    fn start_task_branch(&mut self, module: &BytecodeModule) -> Result<(), Error> {
        let id = self.current;
        let Activation::User(frame) = &self.nodes[id].as_ref().unwrap().activation else {
            unreachable!()
        };
        let Instruction::TaskBranch { target, args } = &frame.function.code[frame.pc] else {
            unreachable!()
        };
        let (target, args) = (*target, args.clone());
        self.frame_capacity(1)?;
        // The capture snapshot happens here: child activation copies the
        // current slot contents, and owning inputs move out of the parent.
        let mut child = Self::activation(module, target, frame, &args)?;
        let owned = Self::owned_arguments(module, target, &args);
        let parent = self.frame_mut(id);
        for (_, src) in &owned {
            parent.cleanups.snapshot(*src)?;
        }
        for (dst, src) in owned {
            if let Some(entry) = parent.cleanups.take(src) {
                match &mut child {
                    Activation::User(frame) => frame.cleanups.insert(dst, entry),
                    Activation::Host(host) => host.cleanups.insert(dst, entry),
                }
            }
        }
        let child_id = self.insert(child, id);
        let frame = self.frame_mut(id);
        let scope = frame
            .task_scopes
            .last_mut()
            .expect("branch requires an open task scope");
        scope.children.push(Some(child_id));
        frame.pc += 1;
        Ok(())
    }

    /// Finish one child or report its Pending to ancestors. A scope then advances
    /// to the next branch rather than starving siblings on a pending first child.
    fn bubble(&mut self, mut value: Option<Value>) -> Option<Poll> {
        loop {
            let id = self.current;
            let parent = self.node(id).parent;
            let complete = value.is_some();
            let mut owner = if complete {
                match &mut self.node_mut(id).activation {
                    Activation::User(frame) => frame
                        .function
                        .owner_result
                        .and_then(|key| frame.cleanups.take(key)),
                    _ => None,
                }
            } else {
                None
            };
            if complete && parent.is_none() {
                let text = matches!(&self.node(id).activation, Activation::User(frame) if frame.function.info.signature.ret == crate::Type::String);
                if text {
                    let Some(Value::Reference(handle)) = value.as_ref() else {
                        return Some(Poll::Failed(abi_error("Invalid owning text result")));
                    };
                    match unsafe { crate::runtime::owned_text::borrow(handle.cast()) }
                        .and_then(crate::runtime::owned_text::copy_to_host)
                    {
                        Ok(text) => value = Some(Value::String(Rc::from(text))),
                        Err(error) => {
                            self.release();
                            return Some(Poll::Failed(error));
                        }
                    }
                    if let Some(entry) = owner.take() {
                        if let Err(error) = unsafe { entry.run(&mut self.arena) } {
                            self.release();
                            return Some(Poll::Failed(error));
                        }
                    }
                }
            }
            if complete {
                self.nodes[id] = None;
                self.free.push(id);
                self.live -= 1;
            }
            let Some(parent) = parent else {
                if complete {
                    self.release();
                }
                return Some(match value {
                    Some(value) => Poll::Complete(value),
                    // A suspended body gives registered dynamic children their
                    // scheduling round before the task surfaces Pending.
                    None => match self.advance_task_round() {
                        Some(child) => {
                            self.current = child;
                            return None;
                        }
                        None => Poll::Pending,
                    },
                });
            };
            self.current = parent;
            let frame = self.frame_mut(parent);
            // Task children report to their registering scope whatever the
            // body is waiting on; ordinary children match the body's pending.
            if Self::finish_task_child(frame, id, complete) {
                if self.round_child == Some(id) {
                    // Continue the child round directly. Re-entering the parent
                    // here would retry its suspended body within the same round.
                    if let Some(child) = self.advance_task_round() {
                        self.current = child;
                        return None;
                    }
                    self.current = 0;
                    return Some(Poll::Pending);
                }
                return None;
            }
            match frame.pending.as_mut().expect("waiting parent") {
                Pending::Call { result, .. } => {
                    if let Some(value) = value.take() {
                        if let Some(slot) = result {
                            frame.slots[*slot] = value;
                            if let Some(entry) = owner {
                                frame.cleanups.insert(*slot, entry);
                            }
                        }
                        frame.pending = None;
                        frame.pc += 1;
                        return None;
                    }
                    // Ordinary calls report Pending up the chain without
                    // moving the caller PC or rebuilding their arguments; the
                    // scheduler round runs once the suspension reaches the
                    // root (see the Pending surfacing in `bubble`).
                }
                Pending::TaskJoin => {
                    unreachable!("task children report to their registering scope")
                }
                Pending::Scope {
                    kind,
                    branches,
                    next,
                    pending,
                } => {
                    let branch = &mut branches[*next];
                    debug_assert_eq!(branch.child, Some(id));
                    if complete {
                        branch.child = None;
                        frame.slots[branch.done_slot] = Value::Bool(true);
                    } else {
                        *pending = true;
                    }
                    *next += 1;
                    if complete && *kind == ScopeKind::Race {
                        let Some(Pending::Scope { branches, .. }) = frame.pending.take() else {
                            unreachable!()
                        };
                        frame.pc += 1;
                        self.cancel_branches(branches);
                    }
                    return None;
                }
            }
        }
    }

    pub(super) fn poll(&mut self, module: &BytecodeModule) -> Poll {
        let _float_env = crate::runtime::float_env::Guard::enter();
        match self.poll_inner(module) {
            Poll::Failed(error) => Poll::Failed(error.with_sources(&module.sources)),
            result => result,
        }
    }

    fn poll_inner(&mut self, module: &BytecodeModule) -> Poll {
        self.cooperative_yield = false;
        if self.live == 0 {
            return Poll::Failed(runtime_error(
                ErrorCode::InvalidState,
                "Interpreter task has already finished",
            ));
        }
        self.budget.slice = if self.asynchronous {
            self.limits.poll_budget
        } else {
            None
        };
        loop {
            // Resume the explicit continuation path. Budget yields leave this
            // cursor intact; host Pending unwinds to the root for the next poll.
            if let Activation::User(frame) =
                &mut self.nodes[self.current].as_mut().unwrap().activation
            {
                match frame.pending.as_mut() {
                    Some(Pending::Call { child, .. }) => {
                        self.current = *child;
                        continue;
                    }
                    Some(Pending::Scope {
                        branches,
                        next,
                        pending,
                        ..
                    }) => {
                        while *next < branches.len() && branches[*next].child.is_none() {
                            *next += 1;
                        }
                        if *next < branches.len() {
                            self.current = branches[*next].child.unwrap();
                            continue;
                        }
                        if *pending {
                            *next = 0;
                            *pending = false;
                            if let Some(poll) = self.bubble(None) {
                                return poll;
                            }
                        } else {
                            frame.pending = None;
                            frame.pc += 1;
                        }
                        continue;
                    }
                    Some(Pending::TaskJoin) => {
                        // Advance the joining scope's children in registration
                        // order; finished children are skipped in place.
                        let ready = {
                            let frame = self.frame_mut(self.current);
                            let scope = frame.task_scopes.last_mut().expect("joining scope");
                            while scope.cursor < scope.children.len()
                                && scope.children[scope.cursor].is_none()
                            {
                                scope.cursor += 1;
                            }
                            scope.cursor < scope.children.len()
                        };
                        if ready {
                            let child = self
                                .frame_mut(self.current)
                                .task_scopes
                                .last()
                                .and_then(|scope| scope.children[scope.cursor])
                                .expect("polled child");
                            self.current = child;
                            continue;
                        }
                        let pending_round = self
                            .frame_mut(self.current)
                            .task_scopes
                            .last()
                            .is_some_and(|scope| {
                                scope.pending && scope.children.iter().any(Option::is_some)
                            });
                        if pending_round {
                            let frame = self.frame_mut(self.current);
                            let scope = frame.task_scopes.last_mut().unwrap();
                            scope.compact();
                            scope.cursor = 0;
                            scope.pending = false;
                            if let Some(poll) = self.bubble(None) {
                                return poll;
                            }
                            continue;
                        }
                        // Every registered child finished: close the scope.
                        let frame = self.frame_mut(self.current);
                        frame.task_scopes.pop();
                        frame.pending = None;
                        frame.pc += 1;
                        continue;
                    }
                    None => {}
                }
            }
            let step = match &mut self.nodes[self.current].as_mut().unwrap().activation {
                Activation::User(frame) => run_frame(
                    frame,
                    &mut self.budget,
                    &mut self.arena,
                    module,
                    &mut self.cleanup_module,
                ),
                Activation::Host(host) => match self.budget.charge() {
                    Charge::Ready => match host.poll(&mut self.arena) {
                        Ok(Some(value)) => match host.finish(&mut self.arena) {
                            Ok(()) if host.aggregate_result().is_some() => Step::CompleteAggregate,
                            Ok(()) => Step::Complete(value),
                            Err(error) => Step::Failed(error),
                        },
                        Ok(None) => {
                            if let Some(poll) = self.bubble(None) {
                                return poll;
                            }
                            continue;
                        }
                        Err(error) => Step::Failed(error),
                    },
                    Charge::Yield => Step::Yield,
                    Charge::Exhausted => Step::Failed(exhausted()),
                },
            };
            match step {
                Step::Call { target, result } => {
                    if let Err(error) = self.start_call(module, target, result) {
                        return self.fail(error);
                    }
                }
                Step::Scope => {
                    if let Err(error) = self.start_scope(module) {
                        return self.fail(error);
                    }
                }
                Step::TaskBranch => {
                    if let Err(error) = self.start_task_branch(module) {
                        return self.fail(error);
                    }
                }
                Step::TaskJoin => {
                    let frame = self.frame_mut(self.current);
                    frame.pending = Some(Pending::TaskJoin);
                }
                Step::CompleteAggregate => {
                    if let Err(error) = self.complete_aggregate() {
                        return self.fail(error);
                    }
                    if let Some(poll) = self.bubble(Some(Value::Unit)) {
                        return poll;
                    }
                }
                Step::Complete(value) => {
                    if let Some(poll) = self.bubble(Some(value)) {
                        return poll;
                    }
                }
                Step::Yield => {
                    self.cooperative_yield = true;
                    return Poll::Pending;
                }
                Step::Failed(error) => return self.fail(error),
            }
        }
    }
}

fn exhausted() -> Error {
    runtime_error(
        ErrorCode::ResourceLimit,
        "Interpreter instruction budget exhausted",
    )
}

fn run_frame(
    frame: &mut Frame,
    budget: &mut Budget,
    arena: &mut crate::runtime::arena::Arena,
    module: &BytecodeModule,
    cleanup_module: &mut Option<Rc<BytecodeModule>>,
) -> Step {
    loop {
        let Some(instruction) = frame.function.code.get(frame.pc) else {
            let result_entry = frame
                .function
                .owner_result
                .and_then(|owner| frame.cleanups.take(owner));
            if let Err(error) = unsafe { frame.cleanups.finish(arena) } {
                if let Some(entry) = result_entry {
                    let _ = unsafe { entry.run(arena) };
                }
                return Step::Failed(error);
            }
            if let Some(entry) = result_entry {
                frame
                    .cleanups
                    .insert(frame.function.owner_result.unwrap(), entry);
            }
            if frame.function.info.signature.ret.is_aggregate() {
                return Step::CompleteAggregate;
            }
            return Step::Complete(
                frame
                    .function
                    .result
                    .map(|slot| frame.slots[slot].clone())
                    .unwrap_or(Value::Unit),
            );
        };
        // Registration completes the preceding ownership transition, including
        // entering a callee and receiving its result. Never pause in between.
        if !matches!(instruction, Instruction::Cleanup { .. }) {
            match budget.charge() {
                Charge::Ready => {}
                Charge::Yield => return Step::Yield,
                Charge::Exhausted => return Step::Failed(exhausted()),
            }
        }
        match instruction {
            Instruction::Cleanup {
                value,
                place,
                hooks,
                elements,
            } => {
                if let Some(place) = place {
                    let Value::Reference(data) = frame.slots[*place] else {
                        unreachable!()
                    };
                    for ((offset, ty), scalar) in value
                        .ty
                        .storage_scalars()
                        .unwrap()
                        .into_iter()
                        .zip(value.scalars())
                    {
                        frame.slots[scalar.index] =
                            unsafe { memory::read_scalar(data.add(offset), &ty) };
                    }
                }
                let owners: Vec<_> = value.scalars().filter(|s| s.ty.is_buffer()).collect();
                let mut entry = crate::runtime::cleanup::Entry {
                    keep_alive: None,
                    storage_size: 0,
                    storage: None,
                    owners: Vec::new(),
                    hooks: vec![],
                };
                for owner in &owners {
                    let Value::Reference(handle) = frame.slots[owner.index] else {
                        return Step::Failed(abi_error("Invalid cleanup owner"));
                    };
                    entry
                        .owners
                        .push(std::ptr::NonNull::new(handle.cast()).map(std::ptr::NonNull::as_ptr));
                }
                if place.is_some() || !hooks.is_empty() {
                    let data = if let Some(place) = place {
                        let Value::Reference(data) = frame.slots[*place] else {
                            unreachable!()
                        };
                        data
                    } else {
                        match memory::snapshot(
                            frame.pc,
                            value,
                            &frame.slots,
                            &mut frame.cleanup_locals,
                        ) {
                            Ok(data) => data,
                            Err(error) => return Step::Failed(error),
                        }
                    };
                    let storage = if place.is_some() {
                        frame.locals.keep_alive()
                    } else {
                        frame.cleanup_locals.keep_alive()
                    };
                    let mut descriptor = crate::runtime::buffer::ElementCleanup {
                        owners: value
                            .ty
                            .storage_scalars()
                            .unwrap()
                            .into_iter()
                            .filter(|(_, ty)| ty.is_buffer())
                            .map(|(offset, _)| offset)
                            .collect(),
                        hooks: vec![],
                    };
                    for hook in hooks {
                        let module = cleanup_module
                            .get_or_insert_with(|| Rc::new(module.clone()))
                            .clone();
                        let name = hook.name.clone();
                        let storage = storage.clone();
                        descriptor.hooks.push(crate::runtime::buffer::ElementHook {
                            before: hook.before,
                            offset: hook.offset,
                            indirect: hook.indirect,
                            guards: hook.guards.clone(),
                            callback: crate::runtime::buffer::ElementCallback::Interpreted(
                                Rc::new(move |data, arena| {
                                    let _keep_alive = &storage;
                                    cleanup_call(&module, &name, data, arena)
                                }),
                            ),
                        });
                    }
                    entry.keep_alive = Some(storage);
                    entry.storage_size = value.ty.storage_layout().unwrap().size;
                    entry.storage = Some((data, descriptor));
                }

                for element in elements {
                    let scalar = value.scalars().nth(element.buffer).unwrap();
                    let Value::Reference(handle) = frame.slots[scalar.index] else {
                        unreachable!()
                    };
                    if handle.is_null() {
                        continue;
                    }
                    let mut cleanup = crate::runtime::buffer::ElementCleanup {
                        owners: element.owners.clone(),
                        hooks: Vec::new(),
                    };
                    for hook in &element.hooks {
                        let module = cleanup_module
                            .get_or_insert_with(|| Rc::new(module.clone()))
                            .clone();
                        let name = hook.name.clone();
                        cleanup.hooks.push(crate::runtime::buffer::ElementHook {
                            before: hook.before,
                            offset: hook.offset,
                            indirect: hook.indirect,
                            guards: hook.guards.clone(),
                            callback: crate::runtime::buffer::ElementCallback::Interpreted(
                                Rc::new(move |data, arena| {
                                    cleanup_call(&module, &name, data, arena)
                                }),
                            ),
                        });
                    }
                    if let Err(error) = unsafe {
                        (*handle.cast::<crate::runtime::buffer::Buffer>()).configure(cleanup)
                    } {
                        // Keep the registration even if descriptor preparation fails.
                        // Existing descriptors remain valid; a new buffer is empty.
                        for owner in &owners {
                            frame.cleanups.take(owner.index);
                        }
                        frame.cleanups.insert(value.owner_key(), entry);
                        return Step::Failed(error);
                    }
                }
                for owner in &owners {
                    frame.cleanups.take(owner.index);
                }
                frame.cleanups.insert(value.owner_key(), entry);
                frame.pc += 1;
            }
            Instruction::Buffer(op) => {
                match &op.op {
                    BufferOp::End(owner) => {
                        if let Err(error) = unsafe {
                            frame
                                .cleanups
                                .end(owner.index, owner.ty.scalar_types().len(), arena)
                        } {
                            return Step::Failed(error);
                        }
                    }
                    BufferOp::Move { dst, src } if dst.index != src.index => {
                        if let Err(error) = unsafe { frame.cleanups.run(dst.owner_key(), arena) } {
                            return Step::Failed(error);
                        }
                        if let Some(entry) = frame.cleanups.take(src.owner_key()) {
                            frame.cleanups.insert(dst.owner_key(), entry);
                        }
                    }
                    BufferOp::Extract { dst, src, owner }
                    | BufferOp::Detach { dst, src, owner } => {
                        let (first, count) = owner.owner_range(src);
                        let entry = frame.cleanups.take_fields(owner.owner_key(), first, count);
                        frame.cleanups.insert(dst.owner_key(), entry);
                    }
                    BufferOp::Replace {
                        dst, src, owner, ..
                    } => {
                        let (first, count) = owner.owner_range(dst);
                        if let Err(error) = unsafe {
                            frame.cleanups.replace_fields(
                                owner.owner_key(),
                                first,
                                count,
                                src.owner_key(),
                                arena,
                            )
                        } {
                            return Step::Failed(error);
                        }
                    }
                    BufferOp::Storage {
                        intrinsic:
                            crate::parser::ast::BufferIntrinsic::Release
                            | crate::parser::ast::BufferIntrinsic::Replace,
                        args,
                        ..
                    } => {
                        for arg in args.iter().filter(|arg| arg.ty.is_owner()) {
                            frame.cleanups.take(arg.owner_key());
                        }
                    }
                    _ => {}
                }
                if let Err(error) =
                    buffers::execute(op, &mut frame.slots, arena, &mut frame.cleanups)
                {
                    return Step::Failed(error);
                }
                if let BufferOp::Storage { args, .. } = &op.op {
                    for arg in args.iter().filter(|arg| arg.ty.is_owner()) {
                        frame.cleanups.take(arg.owner_key());
                    }
                }
                frame.pc += 1;
            }
            Instruction::Memory(op) => {
                if let Some(owner) = memory::moved_owner(op) {
                    let entry = frame
                        .cleanups
                        .take_fields(owner.source, owner.first, owner.count);
                    // Keep the original hooks until registration has successfully
                    // allocated its new snapshot; failure must still drop them.
                    frame.cleanups.insert(owner.destination, entry);
                }
                if let Err(error) = memory::execute(op, &mut frame.slots, arena, &mut frame.locals)
                {
                    return Step::Failed(error);
                }
                frame.pc += 1;
            }
            Instruction::DynCopy { lanes } => {
                for &(dst, src) in lanes {
                    frame.slots[dst] = frame.slots[src].clone();
                }
                frame.pc += 1;
            }
            Instruction::DynMethod {
                dst,
                table,
                method,
                signature,
            } => {
                let Value::DynTable(table) = &frame.slots[*table] else {
                    return Step::Failed(runtime_error(
                        ErrorCode::InvalidCall,
                        "Invalid dyn method table",
                    ));
                };
                let Some((target, _)) = table
                    .entries
                    .get(*method)
                    .filter(|(_, actual)| actual == signature)
                else {
                    return Step::Failed(runtime_error(
                        ErrorCode::InvalidCall,
                        "Invalid dyn method slot or signature",
                    ));
                };
                frame.slots[*dst] = Value::Function(*target);
                frame.pc += 1;
            }
            Instruction::Copy { dst, src } => {
                frame.slots[*dst] = frame.slots[*src].clone();
                frame.pc += 1;
            }
            Instruction::Jump(pc) => frame.pc = *pc,
            Instruction::Branch {
                condition,
                then_pc,
                else_pc,
            } => {
                let Value::Bool(test) = frame.slots[*condition] else {
                    return Step::Failed(runtime_error(
                        ErrorCode::InvalidState,
                        "Bytecode branch received a non-Bool condition",
                    ));
                };
                frame.pc = if test { *then_pc } else { *else_pc };
            }
            Instruction::Text { dst, source } => {
                let bytes: std::borrow::Cow<'_, [u8]> = match source {
                    TextSource::Literal(text) => std::borrow::Cow::Borrowed(text.as_bytes()),
                    TextSource::Clone(src) => {
                        let Value::Reference(owner) = frame.slots[src.index] else {
                            unreachable!()
                        };
                        match unsafe { crate::runtime::owned_text::borrow(owner.cast()) } {
                            Ok(text) => std::borrow::Cow::Borrowed(text.as_bytes()),
                            Err(error) => return Step::Failed(error),
                        }
                    }
                    TextSource::Float(src) => {
                        std::borrow::Cow::Owned(match &frame.slots[src.index] {
                            Value::Float32(value) => format!("{value}").into_bytes(),
                            Value::Float64(value) => format!("{value}").into_bytes(),
                            _ => unreachable!(),
                        })
                    }
                };
                match crate::runtime::buffer::Buffer::copy_bytes(arena, &bytes) {
                    Ok(owner) => frame.slots[*dst] = Value::Reference(owner.as_ptr().cast()),
                    Err(error) => return Step::Failed(error),
                }
                frame.pc += 1;
            }
            Instruction::Const { dst, value } => {
                frame.slots[*dst] = match value {
                    ConstantValue::DynTable(table) => Value::DynTable(table.clone()),
                    ConstantValue::Function(target) => Value::Function(*target),
                    ConstantValue::EmptyReference => Value::Reference(std::ptr::null_mut()),
                    ConstantValue::NullPtr => Value::Ptr(std::ptr::null_mut()),
                    ConstantValue::Float32(value) => Value::Float32(*value),
                    ConstantValue::Float64(value) => Value::Float64(*value),
                    ConstantValue::Int8(value) => Value::Int8(*value),
                    ConstantValue::Int16(value) => Value::Int16(*value),
                    ConstantValue::UInt16(value) => Value::UInt16(*value),
                    ConstantValue::Int32(value) => Value::Int32(*value),
                    ConstantValue::Int64(value) => Value::Int64(*value),
                    ConstantValue::UInt8(value) => Value::UInt8(*value),
                    ConstantValue::UInt32(value) => Value::UInt32(*value),
                    ConstantValue::Rune(value) => Value::Rune(*value),
                    ConstantValue::UInt64(value) => Value::UInt64(*value),
                    ConstantValue::Bool(value) => Value::Bool(*value),
                    ConstantValue::String(value) => Value::String(value.clone()),
                };
                frame.pc += 1;
            }
            Instruction::Require { condition, message } => {
                let Value::Bool(condition) = frame.slots[*condition] else {
                    unreachable!()
                };
                if !condition {
                    let Value::Reference(owner) = frame.slots[*message] else {
                        unreachable!()
                    };
                    let message = match unsafe { crate::runtime::owned_text::borrow(owner.cast()) }
                    {
                        Ok(message) => message,
                        Err(error) => return Step::Failed(error),
                    };
                    return Step::Failed(runtime_error(
                        ErrorCode::RuntimeError,
                        message.to_string(),
                    ));
                }
                frame.pc += 1;
            }
            Instruction::Binary { dst, op, lhs, rhs } => {
                let value = match binary_values(op, &frame.slots[*lhs], &frame.slots[*rhs]) {
                    Ok(value) => value,
                    Err(error) => return Step::Failed(error),
                };
                frame.slots[*dst] = value;
                frame.pc += 1;
            }
            Instruction::Convert {
                dst,
                target,
                operand,
            } => {
                if target.is_float() {
                    macro_rules! cast {
                        ($t:ty, $variant:ident) => {
                            Value::$variant(match &frame.slots[*operand] {
                                Value::Int8(n) => *n as $t,
                                Value::Int16(n) => *n as $t,
                                Value::Int32(n) => *n as $t,
                                Value::Int64(n) => *n as $t,
                                Value::UInt8(n) => *n as $t,
                                Value::UInt16(n) => *n as $t,
                                Value::UInt32(n) => *n as $t,
                                Value::UInt64(n) => *n as $t,
                                Value::Float32(n) => *n as $t,
                                Value::Float64(n) => *n as $t,
                                _ => unreachable!(),
                            })
                        };
                    }
                    frame.slots[*dst] = if *target == Type::Float32 {
                        cast!(f32, Float32)
                    } else {
                        cast!(f64, Float64)
                    };
                    frame.pc += 1;
                    continue;
                }
                // Trusted float sources arrive range-checked by the standard
                // library, so saturating casts never replace a real value.
                if let Value::Float32(value) = &frame.slots[*operand] {
                    frame.slots[*dst] = trunc_float!(*value, target);
                    frame.pc += 1;
                    continue;
                }
                if let Value::Float64(value) = &frame.slots[*operand] {
                    frame.slots[*dst] = trunc_float!(*value, target);
                    frame.pc += 1;
                    continue;
                }
                let bits = match &frame.slots[*operand] {
                    Value::Int8(n) => *n as u64,
                    Value::Int16(n) => *n as u64,
                    Value::Int32(n) => *n as u64,
                    Value::Int64(n) => *n as u64,
                    Value::UInt8(n) => u64::from(*n),
                    Value::UInt16(n) => u64::from(*n),
                    Value::UInt32(n) => u64::from(*n),
                    Value::Rune(n) => u64::from(*n),
                    Value::UInt64(n) => *n,
                    _ => {
                        return Step::Failed(runtime_error(
                            ErrorCode::InvalidState,
                            "Bytecode integer conversion received a non-integer operand",
                        ))
                    }
                };
                frame.slots[*dst] = match target.integer() {
                    Some(IntegerType::Int8) => Value::Int8(bits as u8 as i8),
                    Some(IntegerType::Int16) => Value::Int16(bits as u16 as i16),
                    Some(IntegerType::Int32) => Value::Int32(bits as i32),
                    Some(IntegerType::Int64) => Value::Int64(bits as i64),
                    Some(IntegerType::UInt8) => Value::UInt8(bits as u8),
                    Some(IntegerType::UInt16) => Value::UInt16(bits as u16),
                    Some(IntegerType::UInt32) => Value::UInt32(bits as u32),
                    Some(IntegerType::UInt64) => Value::UInt64(bits),
                    Some(IntegerType::Rune) => Value::Rune(bits as u32),
                    None if *target == Type::Rune => Value::Rune(bits as u32),
                    None => {
                        return Step::Failed(runtime_error(
                            ErrorCode::InvalidState,
                            "Bytecode conversion received an unsupported target type",
                        ))
                    }
                };
                frame.pc += 1;
            }
            Instruction::DomainBits {
                dst,
                src,
                target,
                decode,
            } => {
                let value = frame.slots[*src].instance_bits().and_then(|bits| {
                    if *decode {
                        Value::from_instance_bits(target, bits)
                    } else {
                        Ok(Value::UInt64(bits))
                    }
                });
                match value {
                    Ok(value) => frame.slots[*dst] = value,
                    Err(error) => return Step::Failed(error),
                }
                frame.pc += 1;
            }
            Instruction::Float { dst, src, kind } => {
                let value = match float_primitive(*kind, &frame.slots[*src]) {
                    Ok(value) => value,
                    Err(error) => return Step::Failed(error),
                };
                frame.slots[*dst] = value;
                frame.pc += 1;
            }
            Instruction::Unary { dst, op, operand } => {
                let value = match (op, &frame.slots[*operand]) {
                    (UnaryOp::Neg, Value::Float32(value)) => Value::Float32(-value),
                    (UnaryOp::Neg, Value::Float64(value)) => Value::Float64(-value),
                    (UnaryOp::Neg, Value::Int8(value)) => Value::Int8(value.wrapping_neg()),
                    (UnaryOp::Neg, Value::Int16(value)) => Value::Int16(value.wrapping_neg()),
                    (UnaryOp::Neg, Value::Int32(value)) => Value::Int32(value.wrapping_neg()),
                    (UnaryOp::Neg, Value::Int64(value)) => Value::Int64(value.wrapping_neg()),
                    (UnaryOp::Not, Value::Bool(value)) => Value::Bool(!value),
                    (UnaryOp::BitNot, Value::Int8(value)) => Value::Int8(!value),
                    (UnaryOp::BitNot, Value::Int16(value)) => Value::Int16(!value),
                    (UnaryOp::BitNot, Value::Int32(value)) => Value::Int32(!value),
                    (UnaryOp::BitNot, Value::Int64(value)) => Value::Int64(!value),
                    (UnaryOp::BitNot, Value::UInt8(value)) => Value::UInt8(!value),
                    (UnaryOp::BitNot, Value::UInt16(value)) => Value::UInt16(!value),
                    (UnaryOp::BitNot, Value::UInt32(value)) => Value::UInt32(!value),
                    (UnaryOp::BitNot, Value::UInt64(value)) => Value::UInt64(!value),
                    _ => {
                        return Step::Failed(runtime_error(
                            ErrorCode::InvalidState,
                            "Bytecode unary operation received an invalid operand type",
                        ))
                    }
                };
                frame.slots[*dst] = value;
                frame.pc += 1;
            }
            Instruction::BinaryImm { dst, op, lhs, rhs } => {
                let Value::Int32(lhs) = frame.slots[*lhs] else {
                    return Step::Failed(runtime_error(
                        ErrorCode::InvalidState,
                        "Bytecode binary operation received a non-Int32 operand",
                    ));
                };
                let value = match binary_value(op, lhs, *rhs) {
                    Ok(value) => value,
                    Err(error) => return Step::Failed(error),
                };
                frame.slots[*dst] = value;
                frame.pc += 1;
            }
            Instruction::BranchCompare {
                op,
                lhs,
                rhs,
                then_pc,
                else_pc,
            } => {
                let test = match compare_values(*op, &frame.slots[*lhs], &frame.slots[*rhs]) {
                    Ok(test) => test,
                    Err(error) => return Step::Failed(error),
                };
                frame.pc = if test { *then_pc } else { *else_pc };
            }
            Instruction::BranchCompareImm {
                op,
                lhs,
                rhs,
                then_pc,
                else_pc,
            } => {
                let Value::Int32(lhs) = frame.slots[*lhs] else {
                    return Step::Failed(runtime_error(
                        ErrorCode::InvalidState,
                        "Bytecode binary operation received a non-Int32 operand",
                    ));
                };
                frame.pc = if op.test(i64::from(lhs), i64::from(*rhs)) {
                    *then_pc
                } else {
                    *else_pc
                };
            }
            Instruction::IndirectCall(call) => {
                let Value::Function(target) = frame.slots[call.callee] else {
                    return Step::Failed(runtime_error(
                        ErrorCode::InvalidCall,
                        "Invalid function value",
                    ));
                };
                return Step::Call {
                    target,
                    result: call.result,
                };
            }
            Instruction::Call { target, result, .. } => {
                return Step::Call {
                    target: *target,
                    result: *result,
                }
            }
            Instruction::AggregateCall(call) => {
                return Step::Call {
                    target: call.target,
                    result: None,
                }
            }
            Instruction::Scope { .. } => return Step::Scope,
            Instruction::TaskScopeEnter => {
                frame.task_scopes.push(Default::default());
                frame.pc += 1;
            }
            // Registration is synchronous; the child runs when the body
            // suspends, yields, or joins its scope.
            Instruction::TaskBranch { .. } => return Step::TaskBranch,
            Instruction::TaskJoin => return Step::TaskJoin,
        }
    }
}

#[inline]
fn invalid_operands() -> Error {
    runtime_error(
        ErrorCode::InvalidState,
        "Bytecode binary operation received invalid operand types",
    )
}

#[inline]
fn compare_values(op: Comparison, lhs: &Value, rhs: &Value) -> Result<bool, Error> {
    Ok(match (lhs, rhs) {
        (Value::Float32(a), Value::Float32(b)) => op.test(*a, *b),
        (Value::Float64(a), Value::Float64(b)) => op.test(*a, *b),
        (Value::Int8(a), Value::Int8(b)) => op.test(*a, *b),
        (Value::Int16(a), Value::Int16(b)) => op.test(*a, *b),
        (Value::Int32(a), Value::Int32(b)) => op.test(*a, *b),
        (Value::Int64(a), Value::Int64(b)) => op.test(*a, *b),
        (Value::UInt8(a), Value::UInt8(b)) => op.test(*a, *b),
        (Value::UInt16(a), Value::UInt16(b)) => op.test(*a, *b),
        (Value::UInt32(a), Value::UInt32(b)) => op.test(*a, *b),
        (Value::Rune(a), Value::Rune(b)) => op.test(*a, *b),
        (Value::UInt64(a), Value::UInt64(b)) => op.test(*a, *b),
        (Value::Bool(a), Value::Bool(b)) if matches!(op, Comparison::Eq | Comparison::Ne) => {
            op.test(*a, *b)
        }
        (Value::Ptr(a), Value::Ptr(b)) if matches!(op, Comparison::Eq | Comparison::Ne) => {
            op.test(*a, *b)
        }
        _ => return Err(invalid_operands()),
    })
}

#[inline]
/// Backend-shared IEEE primitives; Rust's f32/f64 methods define the exact
/// NaN, signed-zero, subnormal and rounding behavior every backend mirrors.
fn float_primitive(kind: FloatKind, operand: &Value) -> Result<Value, Error> {
    macro_rules! kernel {
        ($value:expr, $variant:ident) => {
            Value::$variant(match kind {
                FloatKind::Sqrt => $value.sqrt(),
                FloatKind::Floor => $value.floor(),
                FloatKind::Ceil => $value.ceil(),
                FloatKind::Trunc => $value.trunc(),
                FloatKind::RoundTiesEven => $value.round_ties_even(),
                FloatKind::ToBits | FloatKind::FromBits | FloatKind::FromText => {
                    unreachable!("bit reinterpretation takes the matched branch")
                }
            })
        };
    }
    Ok(match operand {
        Value::Reference(owner) if kind == FloatKind::FromText => {
            let text = unsafe { crate::runtime::owned_text::borrow(owner.cast()) }?;
            Value::Float64(crate::runtime::owned_text::parse_float64(text))
        }
        Value::Float32(value) if kind == FloatKind::ToBits => Value::UInt32(value.to_bits()),
        Value::Float64(value) if kind == FloatKind::ToBits => Value::UInt64(value.to_bits()),
        Value::UInt32(bits) if kind == FloatKind::FromBits => Value::Float32(f32::from_bits(*bits)),
        Value::UInt64(bits) if kind == FloatKind::FromBits => Value::Float64(f64::from_bits(*bits)),
        Value::Float32(value) => kernel!(*value, Float32),
        Value::Float64(value) => kernel!(*value, Float64),
        _ => {
            return Err(runtime_error(
                ErrorCode::InvalidState,
                "Bytecode float primitive received a mismatched operand",
            ))
        }
    })
}

fn binary_values(op: &BinOp, lhs: &Value, rhs: &Value) -> Result<Value, Error> {
    match (lhs, rhs) {
        (Value::Ptr(a), Value::Ptr(b)) if matches!(op, BinOp::Eq | BinOp::Ne) => {
            Ok(Value::Bool(if matches!(op, BinOp::Eq) {
                a == b
            } else {
                a != b
            }))
        }
        (Value::Float32(a), Value::Float32(b)) => {
            Ok(match crate::core::float::apply_f32(op, *a, *b) {
                crate::core::float::FloatOutcome::Value(n) => Value::Float32(n),
                crate::core::float::FloatOutcome::Bool(n) => Value::Bool(n),
            })
        }
        (Value::Float64(a), Value::Float64(b)) => {
            Ok(match crate::core::float::apply_f64(op, *a, *b) {
                crate::core::float::FloatOutcome::Value(n) => Value::Float64(n),
                crate::core::float::FloatOutcome::Bool(n) => Value::Bool(n),
            })
        }
        (Value::Int8(a), Value::Int8(b)) => binary_i8(op, *a, *b),
        (Value::Int16(a), Value::Int16(b)) => binary_i16(op, *a, *b),
        (Value::Int32(a), Value::Int32(b)) => binary_value(op, *a, *b),
        (Value::Int64(a), Value::Int64(b)) => binary_value64(op, *a, *b),
        (Value::UInt8(a), Value::UInt8(b)) => binary_u8(op, *a, *b),
        (Value::UInt16(a), Value::UInt16(b)) => binary_u16(op, *a, *b),
        (Value::UInt32(a), Value::UInt32(b)) => binary_u32(op, *a, *b),
        (Value::Rune(a), Value::Rune(b)) if matches!(op, BinOp::Eq | BinOp::Ne) => {
            Ok(Value::Bool(if matches!(op, BinOp::Eq) {
                a == b
            } else {
                a != b
            }))
        }
        (Value::UInt64(a), Value::UInt64(b)) => binary_u64(op, *a, *b),
        (Value::Bool(a), Value::Bool(b)) if matches!(op, BinOp::Eq | BinOp::Ne) => {
            Ok(Value::Bool(if matches!(op, BinOp::Eq) {
                a == b
            } else {
                a != b
            }))
        }
        _ => Err(invalid_operands()),
    }
}

// Keep arithmetic in its concrete width, including the Int32 immediate fast
// path; the wrapping, division and shift rules come from the shared kernel.
macro_rules! integer_binary {
    ($name:ident, $apply:ident, $ty:ty, $variant:ident) => {
        #[inline]
        fn $name(op: &BinOp, lhs: $ty, rhs: $ty) -> Result<Value, Error> {
            match crate::core::integer::$apply(op, lhs, rhs) {
                Ok(IntegerOutcome::Value(value)) => Ok(Value::$variant(value)),
                Ok(IntegerOutcome::Bool(value)) => Ok(Value::Bool(value)),
                Err(trap) => Err(runtime_error(ErrorCode::RuntimeError, trap.message())),
            }
        }
    };
}
integer_binary!(binary_value, apply_i32, i32, Int32);
integer_binary!(binary_value64, apply_i64, i64, Int64);
integer_binary!(binary_i8, apply_i8, i8, Int8);
integer_binary!(binary_i16, apply_i16, i16, Int16);
integer_binary!(binary_u8, apply_u8, u8, UInt8);
integer_binary!(binary_u16, apply_u16, u16, UInt16);
integer_binary!(binary_u32, apply_u32, u32, UInt32);
integer_binary!(binary_u64, apply_u64, u64, UInt64);

struct Task {
    text_result: Option<String>,
    module: Rc<BytecodeModule>,
    execution: Execution,
    result: Option<u64>,
}

impl Drop for Execution {
    fn drop(&mut self) {
        self.release();
    }
}

pub(crate) fn cleanup_call(
    module: &Rc<BytecodeModule>,
    name: &str,
    data: *mut u8,
    arena: &mut crate::runtime::arena::Arena,
) -> Result<(), Error> {
    let function = module.functions[name].clone();
    let args = [Value::Reference(data)];
    let frame = Frame::new(function, args.iter())?;
    // Cleanup must still run after the task exhausts its instruction budget,
    // but an infinite destructor must not make cancellation or failure hang.
    // Give each synchronous hook a bounded, independent cleanup allowance.
    let limits = InterpreterLimits {
        instruction_budget: Some(1_000_000),
        ..InterpreterLimits::default()
    };
    let mut execution = Execution::new(frame, limits, false);
    execution.cleanup_mode = true;
    execution.cleanup_module = Some(module.clone());
    execution.arena = std::mem::take(arena);
    let result = execution.poll(module);
    execution.release();
    *arena = std::mem::take(&mut execution.arena);
    match result {
        Poll::Complete(_) => Ok(()),
        Poll::Failed(error) => Err(error),
        Poll::Pending => Err(abi_error("Destructor cannot suspend")),
    }
}

impl InterpretedTask for Task {
    fn limits(&self) -> InterpreterLimits {
        self.execution.limits
    }
    fn take_text_result(&mut self) -> Option<String> {
        self.text_result.take()
    }
    fn flat_result(&self) -> Option<Vec<u64>> {
        self.execution
            .root_aggregate
            .as_ref()?
            .iter()
            .map(|v| v.instance_bits().ok())
            .collect()
    }
    fn is_text(&self) -> bool {
        self.text_result.is_some()
    }
    fn wait_plan(&self) -> crate::runtime::driver::WaitPlan {
        use crate::runtime::driver::{WaitPlan, Wake};
        let mut plan = WaitPlan::default();
        if self.execution.cooperative_yield {
            plan.add(Some(Wake::Immediate));
        }
        for node in self.execution.nodes.iter().flatten() {
            if let Activation::Host(host) = &node.activation {
                plan.merge(host.wait_plan());
            }
        }
        plan
    }

    fn resume(&mut self) -> Result<(), ResumeError> {
        match self.execution.poll(&self.module) {
            Poll::Pending => Ok(()),
            Poll::Complete(Value::String(text)) => {
                self.text_result = Some(text.to_string());
                self.result = Some(0);
                Ok(())
            }
            Poll::Complete(value) => {
                self.result = Some(value.pack_scalar().ok_or_else(|| {
                    ResumeError::Failed(runtime_error(
                        ErrorCode::InvalidState,
                        "Unexpected async result type",
                    ))
                })?);
                Ok(())
            }
            Poll::Failed(error) => Err(ResumeError::Failed(error)),
        }
    }
    fn is_complete(&self) -> bool {
        self.result.is_some()
    }
    fn result(&self) -> u64 {
        self.result.unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_scope_reclaims_registrations_before_a_long_lived_scope_joins() {
        use crate::runtime::{
            bindings::{HostAbiKind, HostBinding, HostEntry},
            cps::CpsFrame,
        };
        unsafe extern "C" fn pause(frame: *mut CpsFrame) -> i32 {
            if (*frame).state == 0 {
                (*frame).state = 1;
                1
            } else {
                0
            }
        }
        unsafe extern "C" fn park(_: *mut CpsFrame) -> i32 {
            1
        }
        let source = r#"
            extern fn pause() @suspends
            extern fn park() @suspends
            fn main() @suspends {
                scope {
                    branch { park() }
                    var i = 0
                    while i < 4000 {
                        branch {}
                        pause()
                        i = i + 1
                    }
                }
            }
        "#;
        let hosts = [
            ("pause", pause as crate::runtime::cps::CpsEntry),
            ("park", park),
        ]
        .into_iter()
        .map(|(name, entry)| {
            (
                name.into(),
                HostBinding {
                    abi: HostAbiKind::Scalar,
                    signature: crate::FunctionSignature::new(vec![], crate::Type::Unit),
                    entry: HostEntry::Cps {
                        entry,
                        frame_slots: None,
                    },
                    owner: None,
                },
            )
        })
        .collect();
        let ast = crate::parser::parse(source).unwrap();
        let module = compile_with_types(
            &ast,
            &HashMap::new(),
            hosts,
            ModuleId(0),
            &TypeRegistry::default(),
        )
        .unwrap();
        let frame = Frame::new(lookup(&module, "main").unwrap(), [].iter()).unwrap();
        let mut execution = Execution::new(
            frame,
            InterpreterLimits {
                max_frames: 8,
                ..Default::default()
            },
            true,
        );
        for _ in 0..4002 {
            match execution.poll(&module) {
                Poll::Pending => {}
                Poll::Failed(error) => panic!("{error}"),
                Poll::Complete(_) => panic!("the oldest child must still be pending"),
            }
            let scope = &execution.frame_mut(0).task_scopes[0];
            assert_eq!(scope.children.len(), 1);
            assert!(scope.children.capacity() <= 16);
            assert!(execution.nodes.len() <= 8);
        }
        assert!(matches!(
            execution.frame_mut(0).pending,
            Some(Pending::TaskJoin)
        ));
        execution.release();
        assert_eq!(execution.nodes.capacity(), 0);
    }

    #[test]
    fn failure_releases_suspended_sibling_values_before_task_drop() {
        let source = r#"
            fn branch_task(text: String) @suspends { while true {} }
            fn f() @suspends { parallel { | branch_task("owned") | branch_task("owned") } }
        "#;
        let ast = crate::parser::parse(source).unwrap();
        let module = compile_with_types(
            &ast,
            &HashMap::new(),
            HashMap::new(),
            ModuleId(0),
            &TypeRegistry::default(),
        )
        .unwrap();
        let function = lookup(&module, "f").unwrap();
        let constants: Vec<_> = function
            .code
            .iter()
            .filter_map(|op| match op {
                Instruction::Const {
                    value: ConstantValue::String(text),
                    ..
                } => Some(text.clone()),
                _ => None,
            })
            .collect();
        let baseline: Vec<_> = constants.iter().map(Rc::strong_count).collect();
        let frame = Frame::new(function, [].iter()).unwrap();
        let mut execution = Execution::new(
            frame,
            InterpreterLimits {
                instruction_budget: Some(30),
                poll_budget: Some(10),
                ..Default::default()
            },
            true,
        );
        assert!(matches!(execution.poll(&module), Poll::Pending));
        assert!(constants
            .iter()
            .zip(&baseline)
            .all(|(text, baseline)| Rc::strong_count(text) > *baseline));
        let mut failed = false;
        for _ in 0..5 {
            if let Poll::Failed(error) = execution.poll(&module) {
                assert_eq!(error.code(), ErrorCode::ResourceLimit);
                failed = true;
                break;
            }
        }
        assert!(failed);
        assert!(constants
            .iter()
            .zip(&baseline)
            .all(|(text, baseline)| Rc::strong_count(text) == *baseline));
        assert_eq!(execution.nodes.capacity(), 0);
    }
}
