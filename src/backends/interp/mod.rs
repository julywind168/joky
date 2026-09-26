//! Register bytecode interpreter for platforms where executable memory is not
//! available. The bytecode is produced from the same typed plans as JIT/AOT.
use crate::{
    compiler::ir::{Constant, FunctionInfo, ModulePlan, Op, Plan},
    parser::ast::{BinOp, Module, Type},
    runtime::{
        cps::{CpsEntry, CpsFrame, INVALID_STATE},
        string as string_abi,
        suspension::{InterpretedTask, ModuleId, ResumeError},
    },
    Error, FunctionSignature, TypeRegistry,
};
use std::{any::Any, collections::HashMap, rc::Rc};

#[derive(Clone)]
pub(crate) enum HostEntry {
    Direct(*const u8),
    Cps {
        entry: CpsEntry,
        frame_slots: Option<usize>,
    },
}

#[derive(Clone)]
pub(crate) struct HostBinding {
    pub signature: FunctionSignature,
    pub entry: HostEntry,
    pub owner: Option<Rc<dyn Any>>,
}

#[derive(Clone)]
enum ConstantValue {
    Int32(i32),
    Bool(bool),
    String(Rc<str>),
}

#[derive(Clone)]
enum Instruction {
    Const {
        dst: usize,
        value: ConstantValue,
    },
    FunctionRef {
        dst: usize,
    },
    Binary {
        dst: usize,
        op: BinOp,
        lhs: usize,
        rhs: usize,
    },
    Call {
        name: String,
        args: Vec<Option<usize>>,
        result: Option<usize>,
        suspends: bool,
    },
    Scope {
        branches: Vec<BytecodeBranch>,
    },
}

#[derive(Clone)]
struct BytecodeBranch {
    name: String,
    args: Vec<Option<usize>>,
    done: usize,
}

struct BytecodeFunction {
    info: FunctionInfo,
    code: Vec<Instruction>,
    result: Option<usize>,
}

pub(crate) struct BytecodeModule {
    pub(crate) version: ModuleId,
    functions: HashMap<String, Rc<BytecodeFunction>>,
    hosts: HashMap<String, HostBinding>,
    _dependencies: Vec<Rc<dyn Any>>,
}

pub(crate) fn compile_with_types(
    ast: &Module,
    imports: &HashMap<String, usize>,
    hosts: HashMap<String, HostBinding>,
    version: ModuleId,
    types: &TypeRegistry,
) -> Result<BytecodeModule, Error> {
    let plan = ModulePlan::with_import_layouts_and_types(ast, imports, types)?;
    let mut functions = HashMap::new();
    for (name, body) in &plan.bodies {
        let info = plan.functions[name].clone();
        functions.insert(name.clone(), Rc::new(lower_function(&info, body)?));
    }
    let dependencies = hosts
        .values()
        .filter_map(|host| host.owner.clone())
        .collect();
    Ok(BytecodeModule {
        version,
        functions,
        hosts,
        _dependencies: dependencies,
    })
}

fn lower_function(info: &FunctionInfo, plan: &Plan) -> Result<BytecodeFunction, Error> {
    let mut code = Vec::with_capacity(plan.ops.len());
    for op in &plan.ops {
        let instruction = match op {
            Op::Literal(dst, value) => Instruction::Const {
                dst: dst.index,
                value: lower_constant(value),
            },
            Op::FunctionRef(dst, _) => Instruction::FunctionRef { dst: dst.index },
            Op::Binary(dst, op, lhs, rhs) => Instruction::Binary {
                dst: dst.index,
                op: op.clone(),
                lhs: lhs.index,
                rhs: rhs.index,
            },
            Op::Call {
                name,
                args,
                result,
                state,
            } => Instruction::Call {
                name: name.clone(),
                args: args
                    .iter()
                    .map(|slot| slot.as_ref().map(|slot| slot.index))
                    .collect(),
                result: result.as_ref().map(|slot| slot.index),
                suspends: state.is_some(),
            },
            Op::Scope { branches, .. } => Instruction::Scope {
                branches: branches
                    .iter()
                    .map(|branch| BytecodeBranch {
                        name: branch.name.clone(),
                        args: branch
                            .args
                            .iter()
                            .map(|slot| slot.as_ref().map(|slot| slot.index))
                            .collect(),
                        done: branch.done.index,
                    })
                    .collect(),
            },
        };
        code.push(instruction);
    }
    if info.frame_slots != plan.slot_count {
        return Err(Error::new(
            crate::ErrorPhase::Compile,
            format!(
                "Interpreter frame layout mismatch: expected {}, got {}",
                info.frame_slots, plan.slot_count
            ),
        ));
    }
    Ok(BytecodeFunction {
        info: info.clone(),
        code,
        result: plan.result.as_ref().map(|slot| slot.index),
    })
}

fn lower_constant(value: &Constant) -> ConstantValue {
    match value {
        Constant::Int32(value) => ConstantValue::Int32(*value),
        Constant::Bool(value) => ConstantValue::Bool(*value),
        Constant::String(value) => ConstantValue::String(Rc::from(value.as_str())),
    }
}

pub(crate) fn function_signature(module: &BytecodeModule, name: &str) -> Option<FunctionSignature> {
    module
        .functions
        .get(name)
        .map(|function| function.info.signature.clone())
}

pub(crate) fn function_names(module: &BytecodeModule) -> Vec<String> {
    module.functions.keys().cloned().collect()
}

pub(crate) fn call_sync(
    module: &BytecodeModule,
    name: &str,
    args: &[Value],
) -> Result<Value, String> {
    if let Some(function) = module.functions.get(name) {
        if function.info.suspends {
            return Err(format!("Function `{name}` requires call_async"));
        }
        let mut frame = Frame::new(function.clone(), args)?;
        match poll_frame(module, &mut frame) {
            Poll::Complete(value) => Ok(value),
            Poll::Pending => Err(format!("Synchronous function `{name}` suspended")),
            Poll::Failed(code) => Err(format!("Interpreter call failed with status {code}")),
        }
    } else {
        let host = module
            .hosts
            .get(name)
            .ok_or_else(|| format!("Function `{name}` not found"))?;
        if matches!(host.entry, HostEntry::Cps { .. }) {
            return Err(format!("Function `{name}` requires call_async"));
        }
        call_direct(host, args).map_err(|error| format!("Host call failed: {error}"))
    }
}

pub(crate) fn start(
    module: Rc<BytecodeModule>,
    name: &str,
    args: &[i32],
) -> Result<Box<dyn InterpretedTask>, String> {
    let function = module
        .functions
        .get(name)
        .ok_or_else(|| format!("Function `{name}` not found"))?
        .clone();
    if !function.info.suspends {
        return Err(format!("Function `{name}` is not marked @suspends"));
    }
    if !function.info.signature.accepts_async_call(args.len()) {
        return Err(format!("Async call signature mismatch for `{name}`"));
    }
    let values = args.iter().copied().map(Value::Int32).collect::<Vec<_>>();
    let frame = Frame::new(function, &values)?;
    Ok(Box::new(Task {
        module,
        frame: Some(frame),
        complete: false,
        result: 0,
    }))
}

#[derive(Clone)]
pub(crate) enum Value {
    Unit,
    Int32(i32),
    Bool(bool),
    String(Rc<str>),
    Named(u64),
    /// Placeholder for a function reference; the interpreter has no function
    /// ids yet, so this value can only be written, never invoked.
    Function,
}

impl Value {
    fn bits(&self) -> u64 {
        match self {
            Self::Unit => 0,
            Self::Int32(value) => *value as u32 as u64,
            Self::Bool(value) => u64::from(*value),
            Self::Named(value) => *value,
            Self::String(_) | Self::Function => 0,
        }
    }
}

struct Frame {
    function: Rc<BytecodeFunction>,
    slots: Vec<Value>,
    pc: usize,
    pending: Option<Pending>,
}

impl Frame {
    fn new(function: Rc<BytecodeFunction>, args: &[Value]) -> Result<Self, String> {
        if function.info.frame_slots < args.len() {
            return Err("Interpreter frame has too few slots".into());
        }
        let mut slots = vec![Value::Unit; function.info.frame_slots];
        slots[..args.len()].clone_from_slice(args);
        Ok(Self {
            function,
            slots,
            pc: 0,
            pending: None,
        })
    }
}

enum Pending {
    User(Box<Frame>, Option<usize>),
    Host(HostPending, Option<usize>),
    Scope(ScopePending),
}

struct HostPending {
    binding: HostBinding,
    frame: CpsFrame,
    _strings: Vec<Box<[u8]>>,
}

struct ScopePending {
    branches: Vec<BranchPending>,
}

struct BranchPending {
    frame: Frame,
    done: bool,
    done_slot: usize,
}

enum Poll {
    Pending,
    Complete(Value),
    Failed(i32),
}

fn poll_frame(module: &BytecodeModule, frame: &mut Frame) -> Poll {
    loop {
        if let Some(mut pending) = frame.pending.take() {
            let poll = match &mut pending {
                Pending::User(child, result) => poll_frame(module, child).map_value(|value| {
                    if let Some(result) = result {
                        frame.slots[*result] = value.clone();
                    }
                    value
                }),
                Pending::Host(host, result) => poll_host(host).map_value(|value| {
                    if let Some(result) = result {
                        frame.slots[*result] = value.clone();
                    }
                    value
                }),
                Pending::Scope(scope) => poll_scope(module, frame, scope),
            };
            match poll {
                Poll::Pending => {
                    frame.pending = Some(pending);
                    return Poll::Pending;
                }
                Poll::Failed(code) => return Poll::Failed(code),
                Poll::Complete(_) => {
                    frame.pc += 1;
                    continue;
                }
            }
        }

        let Some(instruction) = frame.function.code.get(frame.pc).cloned() else {
            return Poll::Complete(
                frame
                    .function
                    .result
                    .map(|slot| frame.slots[slot].clone())
                    .unwrap_or(Value::Unit),
            );
        };
        match instruction {
            Instruction::Const { dst, value } => {
                frame.slots[dst] = match value {
                    ConstantValue::Int32(value) => Value::Int32(value),
                    ConstantValue::Bool(value) => Value::Bool(value),
                    ConstantValue::String(value) => Value::String(value),
                };
                frame.pc += 1;
            }
            Instruction::FunctionRef { dst } => {
                frame.slots[dst] = Value::Function;
                frame.pc += 1;
            }
            Instruction::Binary { dst, op, lhs, rhs } => {
                let Value::Int32(lhs) = frame.slots[lhs] else {
                    return Poll::Failed(INVALID_STATE);
                };
                let Value::Int32(rhs) = frame.slots[rhs] else {
                    return Poll::Failed(INVALID_STATE);
                };
                let value = match op {
                    BinOp::Add => lhs.wrapping_add(rhs),
                    BinOp::Sub => lhs.wrapping_sub(rhs),
                    BinOp::Mul => lhs.wrapping_mul(rhs),
                    BinOp::Div if rhs != 0 => lhs.wrapping_div(rhs),
                    BinOp::Div => return Poll::Failed(INVALID_STATE),
                };
                frame.slots[dst] = Value::Int32(value);
                frame.pc += 1;
            }
            Instruction::Call {
                name,
                args,
                result,
                suspends,
            } => {
                let values = args
                    .iter()
                    .map(|slot| {
                        slot.map(|slot| frame.slots[slot].clone())
                            .unwrap_or(Value::Unit)
                    })
                    .collect::<Vec<_>>();
                if suspends {
                    match make_pending(module, &name, &values, result) {
                        Ok(pending) => frame.pending = Some(pending),
                        Err(code) => return Poll::Failed(code),
                    }
                } else {
                    match call_sync(module, &name, &values) {
                        Ok(value) => {
                            if let Some(result) = result {
                                frame.slots[result] = value;
                            }
                            frame.pc += 1;
                        }
                        Err(_) => return Poll::Failed(INVALID_STATE),
                    }
                }
            }
            Instruction::Scope { branches } => {
                let mut pending = Vec::with_capacity(branches.len());
                for branch in branches {
                    let values = branch
                        .args
                        .iter()
                        .map(|slot| {
                            slot.map(|slot| frame.slots[slot].clone())
                                .unwrap_or(Value::Unit)
                        })
                        .collect::<Vec<_>>();
                    let Ok(child) = make_user_frame(module, &branch.name, &values) else {
                        return Poll::Failed(INVALID_STATE);
                    };
                    pending.push(BranchPending {
                        frame: child,
                        done: false,
                        done_slot: branch.done,
                    });
                }
                frame.pending = Some(Pending::Scope(ScopePending { branches: pending }));
            }
        }
    }
}

fn make_user_frame(module: &BytecodeModule, name: &str, args: &[Value]) -> Result<Frame, i32> {
    let function = module.functions.get(name).ok_or(INVALID_STATE)?.clone();
    Frame::new(function, args).map_err(|_| INVALID_STATE)
}

fn make_pending(
    module: &BytecodeModule,
    name: &str,
    args: &[Value],
    result: Option<usize>,
) -> Result<Pending, i32> {
    if let Some(function) = module.functions.get(name) {
        return Ok(Pending::User(
            Box::new(Frame::new(function.clone(), args).map_err(|_| INVALID_STATE)?),
            result,
        ));
    }
    let host = module.hosts.get(name).ok_or(INVALID_STATE)?.clone();
    let HostEntry::Cps { frame_slots, .. } = host.entry else {
        return Err(INVALID_STATE);
    };
    let mut frame = CpsFrame::new(frame_slots.unwrap_or(args.len()));
    let mut strings = Vec::new();
    for (index, value) in args.iter().enumerate() {
        frame.slots[index] = encode_host_value(value, &mut strings)?;
    }
    Ok(Pending::Host(
        HostPending {
            binding: host,
            frame,
            _strings: strings,
        },
        result,
    ))
}

fn poll_scope(module: &BytecodeModule, frame: &mut Frame, scope: &mut ScopePending) -> Poll {
    let mut all_done = true;
    for branch in &mut scope.branches {
        if branch.done {
            continue;
        }
        match poll_frame(module, &mut branch.frame) {
            Poll::Pending => all_done = false,
            Poll::Complete(_) => {
                branch.done = true;
                frame.slots[branch.done_slot] = Value::Bool(true);
            }
            Poll::Failed(code) => return Poll::Failed(code),
        }
    }
    if all_done && scope.branches.iter().all(|branch| branch.done) {
        Poll::Complete(Value::Unit)
    } else {
        Poll::Pending
    }
}

fn poll_host(host: &mut HostPending) -> Poll {
    let HostEntry::Cps { entry, .. } = host.binding.entry else {
        return Poll::Failed(INVALID_STATE);
    };
    let status = unsafe { entry(&mut host.frame) };
    match status {
        0 => match decode_host_value(&host.binding.signature.ret, host.frame.result) {
            Ok(value) => Poll::Complete(value),
            Err(code) => Poll::Failed(code),
        },
        1 => Poll::Pending,
        value => Poll::Failed(value),
    }
}

fn decode_host_value(ty: &Type, bits: u64) -> Result<Value, i32> {
    match ty {
        Type::Int32 => Ok(Value::Int32(bits as u32 as i32)),
        Type::Bool => Ok(Value::Bool(bits != 0)),
        Type::Named(_) => Ok(Value::Named(bits)),
        Type::Unit => Ok(Value::Unit),
        Type::String => {
            let text = unsafe { crate::runtime::string::borrow(bits as *const u8) }
                .map_err(|_| INVALID_STATE)?;
            Ok(Value::String(Rc::from(text)))
        }
    }
}

fn encode_host_value(value: &Value, strings: &mut Vec<Box<[u8]>>) -> Result<u64, i32> {
    match value {
        Value::Unit => Ok(0),
        Value::Int32(value) => Ok(*value as u32 as u64),
        Value::Bool(value) => Ok(u64::from(*value)),
        Value::Named(value) => Ok(*value),
        Value::Function => Err(INVALID_STATE),
        Value::String(value) => {
            let bytes = string_abi::encode(value);
            let pointer = bytes.as_ptr() as u64;
            strings.push(bytes);
            Ok(pointer)
        }
    }
}

fn call_direct(host: &HostBinding, args: &[Value]) -> Result<Value, String> {
    let HostEntry::Direct(entry) = host.entry else {
        return Err("suspending host function requires call_async".into());
    };
    if entry.is_null() {
        return Err("null host function entry".into());
    }
    let params = &host.signature.params;
    let ret = &host.signature.ret;
    match (params.as_slice(), ret) {
        ([], Type::Int32) => Ok(Value::Int32(unsafe {
            crate::runtime::abi::call_i32_0(entry)
        })),
        ([Type::Int32], Type::Int32) => Ok(Value::Int32(unsafe {
            crate::runtime::abi::call_i32_1(entry, as_i32(&args[0])?)
        })),
        ([Type::Int32, Type::Int32], Type::Int32) => Ok(Value::Int32(unsafe {
            crate::runtime::abi::call_i32_2(entry, as_i32(&args[0])?, as_i32(&args[1])?)
        })),
        ([], Type::Unit) => {
            unsafe { crate::runtime::abi::call_unit_0(entry) };
            Ok(Value::Unit)
        }
        ([Type::Int32], Type::Unit) => {
            unsafe { crate::runtime::abi::call_unit_1(entry, as_i32(&args[0])?) };
            Ok(Value::Unit)
        }
        ([], Type::Named(_)) => Ok(Value::Named(unsafe {
            crate::runtime::abi::call_named_0(entry)
        })),
        ([Type::Named(_)], Type::Int32) => Ok(Value::Int32(unsafe {
            crate::runtime::abi::call_named_i32(entry, as_named(&args[0])?)
        })),
        ([Type::Named(_), Type::String], Type::Int32) => {
            let bytes = string_bytes(&args[1])?;
            Ok(Value::Int32(unsafe {
                crate::runtime::abi::call_named_string_i32(
                    entry,
                    as_named(&args[0])?,
                    bytes.as_ptr(),
                )
            }))
        }
        ([Type::Named(_), Type::String, Type::Int32], Type::Int32) => {
            let bytes = string_bytes(&args[1])?;
            Ok(Value::Int32(unsafe {
                crate::runtime::abi::call_named_string_i32_i32(
                    entry,
                    as_named(&args[0])?,
                    bytes.as_ptr(),
                    as_i32(&args[2])?,
                )
            }))
        }
        ([Type::String], Type::Int32) => {
            let bytes = string_bytes(&args[0])?;
            Ok(Value::Int32(unsafe {
                crate::runtime::abi::call_string_i32(entry, bytes.as_ptr())
            }))
        }
        ([], Type::String) => Ok(Value::String(decode_string_ptr(unsafe {
            crate::runtime::abi::call_string_0(entry)
        })?)),
        ([Type::String], Type::String) => {
            let bytes = string_bytes(&args[0])?;
            Ok(Value::String(decode_string_ptr(unsafe {
                crate::runtime::abi::call_string_string(entry, bytes.as_ptr())
            })?))
        }
        _ => Err(format!(
            "unsupported interpreter host signature ({:?}) -> {}",
            params, ret
        )),
    }
}

fn string_bytes(value: &Value) -> Result<Box<[u8]>, String> {
    let Value::String(value) = value else {
        return Err("expected String host argument".into());
    };
    Ok(string_abi::encode(value))
}

fn as_i32(value: &Value) -> Result<i32, String> {
    match value {
        Value::Int32(value) => Ok(*value),
        _ => Err("expected Int32 host argument".into()),
    }
}

fn as_named(value: &Value) -> Result<u64, String> {
    match value {
        Value::Named(value) => Ok(*value),
        _ => Err("expected named host argument".into()),
    }
}

fn decode_string_ptr(pointer: *const u8) -> Result<Rc<str>, String> {
    let text = unsafe { crate::runtime::string::borrow(pointer) }?;
    Ok(Rc::from(text))
}

struct Task {
    module: Rc<BytecodeModule>,
    frame: Option<Frame>,
    complete: bool,
    result: u64,
}

impl InterpretedTask for Task {
    fn resume(&mut self) -> Result<(), ResumeError> {
        let Some(frame) = self.frame.as_mut() else {
            return Err(ResumeError::AlreadyFinished);
        };
        match poll_frame(&self.module, frame) {
            Poll::Pending => Ok(()),
            Poll::Complete(value) => {
                self.result = value.bits();
                self.complete = true;
                self.frame = None;
                Ok(())
            }
            Poll::Failed(code) => Err(ResumeError::InvalidCpsStatus(code)),
        }
    }

    fn is_complete(&self) -> bool {
        self.complete
    }

    fn result(&self) -> u64 {
        self.result
    }
}

trait PollValue {
    fn map_value(self, map: impl FnOnce(Value) -> Value) -> Self;
}

impl PollValue for Poll {
    fn map_value(self, map: impl FnOnce(Value) -> Value) -> Self {
        match self {
            Poll::Complete(value) => Poll::Complete(map(value)),
            other => other,
        }
    }
}
