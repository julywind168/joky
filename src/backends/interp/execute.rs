//! Iterative execution: language calls and scopes never recurse on the Rust stack.
use super::{host::HostPending, *};
use crate::{
    core::integer::IntegerOutcome,
    runtime::suspension::{InterpretedTask, ResumeError},
    vm::InterpreterLimits,
    RuntimeFrame,
};

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
    match execution.poll(module) {
        Poll::Complete(value) => Ok(value),
        Poll::Failed(error) => Err(error),
        Poll::Pending => Err(runtime_error(
            ErrorCode::InvalidState,
            "Synchronous function suspended",
        )),
    }
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
    function: Rc<BytecodeFunction>,
    slots: Vec<Value>,
    pc: usize,
    pending: Option<Pending>,
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
            function,
            slots,
            pc: 0,
            pending: None,
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
        branches: Vec<Branch>,
        next: usize,
        pending: bool,
    },
}

struct Branch {
    child: Option<usize>,
    done_slot: usize,
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
    nodes: Vec<Option<Node>>,
    free: Vec<usize>,
    live: usize,
    current: usize,
    limits: InterpreterLimits,
    budget: Budget,
    asynchronous: bool,
    arena: crate::runtime::arena::Arena,
}

impl Execution {
    pub(super) fn new(frame: Frame, limits: InterpreterLimits, asynchronous: bool) -> Self {
        Self {
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
        self.nodes = Vec::new();
        self.free = Vec::new();
        self.live = 0;
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

    fn start_call(
        &mut self,
        module: &BytecodeModule,
        target: usize,
        result: Option<usize>,
    ) -> Result<(), Error> {
        let id = self.current;
        let Activation::User(frame) = &self.node(id).activation else {
            unreachable!()
        };
        let args = match &frame.function.code[frame.pc] {
            Instruction::Call { args, .. } => args,
            Instruction::AggregateCall(call) => &call.args,
            _ => unreachable!(),
        };
        if let Callee::Host(host) = &module.callees[target] {
            if !host.suspends() {
                let value = host.call(Self::arguments(frame, args))?;
                let frame = self.frame_mut(id);
                if let Some(slot) = result {
                    frame.slots[slot] = value;
                }
                frame.pc += 1;
                return Ok(());
            }
        }
        self.frame_capacity(1)?;
        let child = Self::activation(module, target, frame, args)?;
        let child = self.insert(child, id);
        self.frame_mut(id).pending = Some(Pending::Call { child, result });
        self.current = child;
        Ok(())
    }

    /// Copy flattened results while both activations are alive. This keeps
    /// aggregates out of the scalar Value enum and needs no aggregate allocation.
    fn complete_aggregate(&mut self) -> Result<(), Error> {
        let child_id = self.current;
        let parent_id = self.node(child_id).parent.ok_or_else(|| {
            runtime_error(
                ErrorCode::InvalidCall,
                "Aggregate results require an internal language caller",
            )
        })?;
        let (child, parent) = if child_id < parent_id {
            let (left, right) = self.nodes.split_at_mut(parent_id);
            (&left[child_id], &mut right[0])
        } else {
            let (left, right) = self.nodes.split_at_mut(child_id);
            (&right[0], &mut left[parent_id])
        };
        let Activation::User(child) = &child.as_ref().unwrap().activation else {
            unreachable!()
        };
        let Activation::User(parent) = &mut parent.as_mut().unwrap().activation else {
            unreachable!()
        };
        let Instruction::AggregateCall(call) = &parent.function.code[parent.pc] else {
            unreachable!()
        };
        debug_assert_eq!(call.results.len(), child.function.aggregate_result.len());
        for (&dst, &src) in call.results.iter().zip(&child.function.aggregate_result) {
            parent.slots[dst] = child.slots[src].clone();
        }
        Ok(())
    }

    fn start_scope(&mut self, module: &BytecodeModule) -> Result<(), Error> {
        let id = self.current;
        let Activation::User(frame) = &self.node(id).activation else {
            unreachable!()
        };
        let Instruction::Scope { branches } = &frame.function.code[frame.pc] else {
            unreachable!()
        };
        self.frame_capacity(branches.len())?;
        // Construct all owned argument snapshots before polling any branch.
        let children: Vec<_> = branches
            .iter()
            .map(|branch| {
                Self::activation(module, branch.target, frame, &branch.args)
                    .map(|child| (child, branch.done))
            })
            .collect::<Result<_, _>>()?;
        let branches = children
            .into_iter()
            .map(|(child, done_slot)| Branch {
                child: Some(self.insert(child, id)),
                done_slot,
            })
            .collect();
        self.frame_mut(id).pending = Some(Pending::Scope {
            branches,
            next: 0,
            pending: false,
        });
        Ok(())
    }

    /// Finish one child or report its Pending to ancestors. A scope then advances
    /// to the next branch rather than starving siblings on a pending first child.
    fn bubble(&mut self, mut value: Option<Value>) -> Option<Poll> {
        loop {
            let id = self.current;
            let parent = self.node(id).parent;
            let complete = value.is_some();
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
                    None => Poll::Pending,
                });
            };
            self.current = parent;
            let frame = self.frame_mut(parent);
            match frame.pending.as_mut().expect("waiting parent") {
                Pending::Call { result, .. } => {
                    if let Some(value) = value.take() {
                        if let Some(slot) = result {
                            frame.slots[*slot] = value;
                        }
                        frame.pending = None;
                        frame.pc += 1;
                        return None;
                    }
                    // Ordinary calls report Pending up the chain without moving
                    // the caller PC or rebuilding their arguments.
                }
                Pending::Scope {
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
                    return None;
                }
            }
        }
    }

    pub(super) fn poll(&mut self, module: &BytecodeModule) -> Poll {
        match self.poll_inner(module) {
            Poll::Failed(error) => Poll::Failed(error.with_sources(&module.sources)),
            result => result,
        }
    }

    fn poll_inner(&mut self, module: &BytecodeModule) -> Poll {
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
                    None => {}
                }
            }
            let step = match &mut self.nodes[self.current].as_mut().unwrap().activation {
                Activation::User(frame) => run_frame(frame, &mut self.budget, &mut self.arena),
                Activation::Host(host) => match self.budget.charge() {
                    Charge::Ready => match host.poll() {
                        Ok(Some(value)) => Step::Complete(value),
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
                Step::Yield => return Poll::Pending,
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
) -> Step {
    loop {
        let Some(instruction) = frame.function.code.get(frame.pc) else {
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
        match budget.charge() {
            Charge::Ready => {}
            Charge::Yield => return Step::Yield,
            Charge::Exhausted => return Step::Failed(exhausted()),
        }
        match instruction {
            Instruction::Memory(op) => {
                if let Err(error) = memory::execute(op, &mut frame.slots, arena) {
                    return Step::Failed(error);
                }
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
            Instruction::Const { dst, value } => {
                frame.slots[*dst] = match value {
                    ConstantValue::Int32(value) => Value::Int32(*value),
                    ConstantValue::Int64(value) => Value::Int64(*value),
                    ConstantValue::UInt32(value) => Value::UInt32(*value),
                    ConstantValue::UInt64(value) => Value::UInt64(*value),
                    ConstantValue::Bool(value) => Value::Bool(*value),
                    ConstantValue::String(value) => Value::String(value.clone()),
                };
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
                let bits = match &frame.slots[*operand] {
                    Value::Int32(n) => *n as u64,
                    Value::Int64(n) => *n as u64,
                    Value::UInt32(n) => u64::from(*n),
                    Value::UInt64(n) => *n,
                    _ => {
                        return Step::Failed(runtime_error(
                            ErrorCode::InvalidState,
                            "Bytecode integer conversion received a non-integer operand",
                        ))
                    }
                };
                frame.slots[*dst] = match target {
                    IntegerType::Int32 => Value::Int32(bits as i32),
                    IntegerType::Int64 => Value::Int64(bits as i64),
                    IntegerType::UInt32 => Value::UInt32(bits as u32),
                    IntegerType::UInt64 => Value::UInt64(bits),
                };
                frame.pc += 1;
            }
            Instruction::Unary { dst, op, operand } => {
                let value = match (op, &frame.slots[*operand]) {
                    (UnaryOp::Neg, Value::Int32(value)) => Value::Int32(value.wrapping_neg()),
                    (UnaryOp::Neg, Value::Int64(value)) => Value::Int64(value.wrapping_neg()),
                    (UnaryOp::Not, Value::Bool(value)) => Value::Bool(!value),
                    (UnaryOp::BitNot, Value::Int32(value)) => Value::Int32(!value),
                    (UnaryOp::BitNot, Value::Int64(value)) => Value::Int64(!value),
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
        (Value::Int32(a), Value::Int32(b)) => op.test(*a, *b),
        (Value::Int64(a), Value::Int64(b)) => op.test(*a, *b),
        (Value::UInt32(a), Value::UInt32(b)) => op.test(*a, *b),
        (Value::UInt64(a), Value::UInt64(b)) => op.test(*a, *b),
        (Value::Bool(a), Value::Bool(b)) if matches!(op, Comparison::Eq | Comparison::Ne) => {
            op.test(*a, *b)
        }
        _ => return Err(invalid_operands()),
    })
}

#[inline]
fn binary_values(op: &BinOp, lhs: &Value, rhs: &Value) -> Result<Value, Error> {
    match (lhs, rhs) {
        (Value::Int32(a), Value::Int32(b)) => binary_value(op, *a, *b),
        (Value::Int64(a), Value::Int64(b)) => binary_value64(op, *a, *b),
        (Value::UInt32(a), Value::UInt32(b)) => binary_u32(op, *a, *b),
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
integer_binary!(binary_u32, apply_u32, u32, UInt32);
integer_binary!(binary_u64, apply_u64, u64, UInt64);

struct Task {
    module: Rc<BytecodeModule>,
    execution: Execution,
    result: Option<u64>,
}

impl InterpretedTask for Task {
    fn resume(&mut self) -> Result<(), ResumeError> {
        match self.execution.poll(&self.module) {
            Poll::Pending => Ok(()),
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
    fn failure_releases_suspended_sibling_values_before_task_drop() {
        let source = r#"
            fn branch(text: String) @suspends { while true {} }
            fn f() @suspends { scope { | branch("owned") | branch("owned") } }
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
