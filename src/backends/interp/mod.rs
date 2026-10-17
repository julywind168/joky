//! Register bytecode interpreter for platforms where executable memory is not
//! available. The bytecode is produced from the same typed plans as JIT/AOT.
use crate::{
    core::{
        integer::{Comparison, IntegerType},
        ir::{
            BufferOp, CleanupOp, Constant, DropHook, FloatKind, FunctionInfo, FunctionPlan, Op,
            TextSource,
        },
        prepared::PreparedModule,
    },
    parser::ast::{BinOp, ScopeKind, UnaryOp},
    runtime::{bindings::HostBinding, suspension::ModuleId},
    Error, ErrorCode, ErrorPhase, FunctionSignature, Span, Type,
};
use std::{
    collections::{BTreeSet, HashMap},
    rc::Rc,
};

#[cfg(test)]
use crate::{core::ir::ModulePlan, TypeRegistry};

mod buffers;
#[cfg(test)]
mod diagnostics;
#[cfg(test)]
mod dyn_tests;
mod execute;
mod host;
mod memory;
mod optimize;

pub(crate) use execute::{
    call_instance, call_sync, cleanup_call, prepare_scalar, prepare_script, start, start_text,
};
#[cfg(test)]
use execute::{Execution, Frame, Poll};
use host::BoundHost;

/// Immutable, compiler-validated IDs and slot signatures; never raw entry addresses.
pub(crate) struct DynMethodTable {
    entries: Vec<(usize, crate::FunctionSignature)>,
}

enum ConstantValue {
    DynTable(Rc<DynMethodTable>),
    Function(usize),
    NullPtr,
    EmptyReference,
    Float32(f32),
    Float64(f64),
    Int8(i8),
    Int16(i16),
    Int32(i32),
    Int64(i64),
    UInt8(u8),
    UInt16(u16),
    UInt32(u32),
    Rune(u32),
    UInt64(u64),
    Bool(bool),
    String(Rc<str>),
}

enum Instruction {
    DomainBits {
        dst: usize,
        src: usize,
        target: Type,
        decode: bool,
    },
    DynCopy {
        lanes: Vec<(usize, usize)>,
    },
    DynMethod {
        dst: usize,
        table: usize,
        method: usize,
        signature: crate::FunctionSignature,
    },
    Text {
        dst: usize,
        source: TextSource,
    },
    Assert {
        condition: usize,
        message: usize,
    },
    Panic {
        message: usize,
    },
    Cleanup {
        value: crate::core::ir::Slot,
        place: Option<usize>,
        hooks: Vec<DropHook>,
        elements: Vec<crate::core::ir::ElementCleanup>,
    },
    IndirectCall(Box<IndirectCall>),
    Buffer(Box<buffers::Instruction>),
    Memory(Box<memory::Instruction>),
    Copy {
        dst: usize,
        src: usize,
    },
    Jump(usize),
    Branch {
        condition: usize,
        then_pc: usize,
        else_pc: usize,
    },
    Const {
        dst: usize,
        value: ConstantValue,
    },
    Binary {
        dst: usize,
        op: BinOp,
        lhs: usize,
        rhs: usize,
    },
    Convert {
        dst: usize,
        target: Type,
        operand: usize,
    },
    Float {
        dst: usize,
        src: usize,
        kind: FloatKind,
    },
    Unary {
        dst: usize,
        op: UnaryOp,
        operand: usize,
    },
    BinaryImm {
        dst: usize,
        op: BinOp,
        lhs: usize,
        rhs: i32,
    },
    BranchCompare {
        op: Comparison,
        lhs: usize,
        rhs: usize,
        then_pc: usize,
        else_pc: usize,
    },
    BranchCompareImm {
        op: Comparison,
        lhs: usize,
        rhs: i32,
        then_pc: usize,
        else_pc: usize,
    },
    Call {
        target: usize,
        args: Vec<Option<usize>>,
        result: Option<usize>,
    },
    AggregateCall(Box<AggregateCall>),
    Scope {
        result: Option<usize>,
        results: Vec<usize>,
        kind: ScopeKind,
        branches: Vec<BytecodeBranch>,
    },
    ScopeFinish,
    /// Open a dynamic task scope on the current frame.
    TaskScopeEnter,
    /// Register a child task entry with captured inputs; the child runs when
    /// the body suspends, yields, or joins.
    TaskBranch {
        target: usize,
        args: Vec<Option<usize>>,
    },
    /// Suspend into the top scope's join until every registered child ends.
    TaskJoin,
}

struct IndirectCall {
    callee: usize,
    args: Vec<Option<usize>>,
    result: Option<usize>,
    results: Vec<usize>,
}

struct AggregateCall {
    target: usize,
    args: Vec<Option<usize>>,
    results: Vec<usize>,
}

struct BytecodeBranch {
    result: Option<usize>,
    results: Vec<usize>,
    indirect: Option<usize>,
    target: Option<usize>,
    args: Vec<Option<usize>>,
    done: usize,
}

struct BytecodeFunction {
    name: String,
    spans: Vec<Span>,
    info: FunctionInfo,
    code: Vec<Instruction>,
    result: Option<usize>,
    aggregate_result: Vec<usize>,
    owner_result: Option<usize>,
    instance_hooks: Vec<DropHook>,
}

#[derive(Clone)]
pub(crate) struct BytecodeModule {
    pub(crate) version: ModuleId,
    pub(crate) files: Option<crate::runtime::fs::FileService>,
    sources: std::sync::Arc<crate::SourceMap>,
    functions: HashMap<String, Rc<BytecodeFunction>>,
    callees: Vec<Callee>,
}

#[derive(Clone)]
enum Callee {
    User(Rc<BytecodeFunction>),
    Host(Rc<BoundHost>),
}

pub(crate) fn compile(
    prepared: &PreparedModule,
    imports: &HashMap<String, usize>,
    hosts: HashMap<String, HostBinding>,
    version: ModuleId,
) -> Result<BytecodeModule, Error> {
    let _float_env = crate::runtime::float_env::Guard::enter();
    let plan = prepared.plan(imports)?;
    // Resolve every static call before publishing the module. Unused injected
    // imports need no adapter; merely importing joky/vm must remain supported.
    let mut names: BTreeSet<String> = plan.bodies.keys().cloned().collect();
    let mut bound_hosts = HashMap::new();
    let mut body_names: Vec<_> = plan.bodies.keys().collect();
    body_names.sort();
    for name in body_names {
        let body = &plan.bodies[name];
        for (op, span) in body.ops.iter().zip(&body.op_spans) {
            let targets: Vec<&str> = match op {
                Op::DynTable { table, .. } => table.methods.iter().map(String::as_str).collect(),
                Op::Call {
                    name,
                    indirect: None,
                    ..
                } => vec![name],
                Op::FunctionRef(slot, name) if matches!(slot.ty, crate::Type::Function(_)) => {
                    vec![name]
                }
                Op::Scope { branches, .. } => branches
                    .iter()
                    .filter(|b| b.indirect.is_none())
                    .map(|b| b.name.as_str())
                    .collect(),
                Op::FunctionRef(_, _) => {
                    return Err(abi_error(
                        "Native function references are unavailable in interpreter mode",
                    )
                    .with_span(*span))
                }
                _ => continue,
            };
            for name in targets {
                names.insert(name.to_owned());
                if !plan.bodies.contains_key(name) && !bound_hosts.contains_key(name) {
                    let binding = hosts.get(name).ok_or_else(|| {
                        abi_error(format!("Host function `{name}` is not registered"))
                            .with_span(*span)
                    })?;
                    let host = BoundHost::new(name, binding.clone())
                        .map_err(|error| error.with_span(*span))?;
                    bound_hosts.insert(name.to_owned(), Rc::new(host));
                }
            }
        }
    }
    let ids: HashMap<_, _> = names
        .iter()
        .enumerate()
        .map(|(id, name)| (name.clone(), id))
        .collect();
    let mut functions = HashMap::new();
    let mut tables = HashMap::new();
    for name in &names {
        if let Some(body) = plan.bodies.get(name) {
            let mut function = lower_function(&plan.functions[name], body, &ids, &mut tables)?;
            function.name.clone_from(name);
            functions.insert(name.clone(), Rc::new(function));
        }
    }
    let callees = names
        .iter()
        .map(|name| {
            if let Some(function) = functions.get(name) {
                Callee::User(function.clone())
            } else {
                Callee::Host(bound_hosts.remove(name).expect("validated host binding"))
            }
        })
        .collect();
    Ok(BytecodeModule {
        files: None,
        sources: prepared.ast().sources.clone(),
        version,
        functions,
        callees,
    })
}

/// Test adapter for compiling a source AST with an explicit type environment.
#[cfg(test)]
pub(crate) fn compile_with_types(
    ast: &crate::parser::ast::Module,
    imports: &HashMap<String, usize>,
    hosts: HashMap<String, HostBinding>,
    version: ModuleId,
    types: &crate::TypeRegistry,
) -> Result<BytecodeModule, Error> {
    let prepared = PreparedModule::new(ast, types, &Default::default())?;
    compile(&prepared, imports, hosts, version)
}

fn lower_function(
    info: &FunctionInfo,
    plan: &FunctionPlan,
    ids: &HashMap<String, usize>,
    tables: &mut HashMap<usize, Rc<DynMethodTable>>,
) -> Result<BytecodeFunction, Error> {
    let mut function = lower_unoptimized(info, plan, ids, tables)?;
    // The current optimizer reasons about values, not addressable aliasing.
    if !info.needs_arena && !plan.has_references() && !plan.has_buffers() {
        optimize::optimize(&mut function);
        optimize::compact_slots(&mut function);
    }
    Ok(function)
}

fn lower_unoptimized(
    info: &FunctionInfo,
    plan: &FunctionPlan,
    ids: &HashMap<String, usize>,
    tables: &mut HashMap<usize, Rc<DynMethodTable>>,
) -> Result<BytecodeFunction, Error> {
    // Labels are compile-time markers, not executable instructions. Consecutive
    // labels share a PC; a terminal label maps to code.len() for completion.
    // Resume PCs below count emitted instructions, independently of IR states.
    let mut labels = vec![0; plan.label_count];
    let mut pc = 0;
    for op in &plan.ops {
        if let Op::Label(label) = op {
            labels[*label] = pc;
        } else {
            pc += 1;
        }
    }
    let mut code = Vec::with_capacity(pc);
    let mut spans = Vec::with_capacity(pc);
    for (op, span) in plan.ops.iter().zip(&plan.op_spans) {
        let instruction = match op {
            Op::Closure(_) => unreachable!("closure responsibility is erased after checking"),
            Op::Cleanup(CleanupOp::Register {
                value,
                place,
                hooks,
                elements,
            }) => Instruction::Cleanup {
                value: value.clone(),
                place: place.as_ref().map(|s| s.index),
                hooks: hooks.clone(),
                elements: elements.clone(),
            },
            Op::Text { dst, source } => Instruction::Text {
                dst: dst.index,
                source: source.clone(),
            },
            Op::Buffer(op) => Instruction::Buffer(Box::new(buffers::lower(op)?)),
            Op::Graph(_) => unreachable!("construction operations are erased after checking"),
            Op::EndLocal(_) => unreachable!("local lifetime markers are erased after checking"),
            Op::ArenaNew { .. }
            | Op::ArenaAlloc { .. }
            | Op::Alloc { .. }
            | Op::Local { .. }
            | Op::Project { .. }
            | Op::PatternProject { .. }
            | Op::ReadMemory { .. }
            | Op::ReadOwner { .. }
            | Op::StoreOwner { .. }
            | Op::WriteMemory { .. } => Instruction::Memory(Box::new(memory::lower(op)?)),
            Op::Copy(dst, src) | Op::Reference { dst, src, .. } | Op::Reserve { dst, src } => {
                let lanes: Vec<_> = dst
                    .scalars()
                    .zip(src.scalars())
                    .map(|(dst, src)| (dst.index, src.index))
                    .collect();
                if lanes.len() > 1 {
                    Instruction::DynCopy { lanes }
                } else {
                    Instruction::Copy {
                        dst: dst.index,
                        src: src.index,
                    }
                }
            }
            Op::DynTable { dst, table } => Instruction::Const {
                dst: dst.index,
                value: ConstantValue::DynTable(
                    tables
                        .entry(table.id)
                        .or_insert_with(|| {
                            Rc::new(DynMethodTable {
                                entries: table
                                    .methods
                                    .iter()
                                    .zip(&table.interface.methods)
                                    .map(|(name, slot)| (ids[name], slot.signature.clone()))
                                    .collect(),
                            })
                        })
                        .clone(),
                ),
            },
            Op::DynMethod { dst, table, method } => Instruction::DynMethod {
                dst: dst.index,
                table: table.index,
                method: *method,
                signature: match &dst.ty {
                    crate::Type::Function(f) => f.signature.clone(),
                    _ => unreachable!("typed dyn entry"),
                },
            },
            Op::Label(_) => continue,
            Op::Jump(label) => Instruction::Jump(labels[*label]),
            Op::Branch {
                condition,
                then_label,
                else_label,
            } => Instruction::Branch {
                condition: condition.index,
                then_pc: labels[*then_label],
                else_pc: labels[*else_label],
            },
            Op::Literal(dst, value) => Instruction::Const {
                dst: dst.index,
                value: if matches!(dst.ty, crate::Type::Function(_)) {
                    ConstantValue::Function(usize::MAX)
                } else if dst.ty.reference_type().is_some() || dst.ty.is_buffer() {
                    ConstantValue::EmptyReference
                } else {
                    lower_constant(value)
                },
            },
            Op::DomainBits { dst, src, decode } => Instruction::DomainBits {
                dst: dst.index,
                src: src.index,
                target: dst.ty.clone(),
                decode: *decode,
            },
            Op::DomainModule(slot) => Instruction::Const {
                dst: slot.index,
                value: ConstantValue::UInt64(0),
            },
            Op::FunctionRef(slot, name) if matches!(slot.ty, crate::Type::Function(_)) => {
                Instruction::Const {
                    dst: slot.index,
                    value: ConstantValue::Function(ids[name]),
                }
            }
            Op::FunctionRef(_, _) => {
                // The callee scan in compile rejects native registration
                // references before any lowering runs.
                unreachable!("native function reference survived the callee scan")
            }
            Op::Assert { condition, message } => Instruction::Assert {
                condition: condition.index,
                message: message.index,
            },
            Op::Panic { message } => Instruction::Panic {
                message: message.index,
            },
            Op::Binary(dst, op, lhs, rhs) => Instruction::Binary {
                dst: dst.index,
                op: op.clone(),
                lhs: lhs.index,
                rhs: rhs.index,
            },
            Op::Convert(dst, src) => Instruction::Convert {
                dst: dst.index,
                target: dst.ty.clone(),
                operand: src.index,
            },
            Op::Float { dst, src, kind } => Instruction::Float {
                dst: dst.index,
                src: src.index,
                kind: *kind,
            },
            Op::Unary(dst, op, operand) => Instruction::Unary {
                dst: dst.index,
                op: op.clone(),
                operand: operand.index,
            },
            Op::Call {
                name,
                args,
                result,
                indirect,
                ..
            } => {
                let args = args
                    .iter()
                    .map(|slot| slot.as_ref().map(|slot| slot.index))
                    .collect();
                if let Some(target) = indirect {
                    Instruction::IndirectCall(Box::new(IndirectCall {
                        callee: target.index,
                        args,
                        result: result
                            .as_ref()
                            .filter(|s| !s.ty.is_aggregate())
                            .map(|s| s.index),
                        results: result
                            .as_ref()
                            .filter(|s| s.ty.is_aggregate())
                            .map_or_else(Vec::new, |s| s.scalars().map(|s| s.index).collect()),
                    }))
                } else if let Some(result) = result.as_ref().filter(|slot| slot.ty.is_aggregate()) {
                    Instruction::AggregateCall(Box::new(AggregateCall {
                        target: ids[name],
                        args,
                        results: result.scalars().map(|slot| slot.index).collect(),
                    }))
                } else {
                    Instruction::Call {
                        target: ids[name],
                        args,
                        result: result.as_ref().map(|slot| slot.index),
                    }
                }
            }
            Op::Scope {
                kind,
                branches,
                result,
                ..
            } => Instruction::Scope {
                result: result
                    .as_ref()
                    .filter(|s| !s.ty.is_aggregate())
                    .map(|s| s.index),
                results: result
                    .as_ref()
                    .filter(|s| s.ty.is_aggregate())
                    .map_or_else(Vec::new, |s| s.scalars().map(|s| s.index).collect()),
                kind: *kind,
                branches: branches
                    .iter()
                    .map(|branch| BytecodeBranch {
                        result: branch
                            .result
                            .as_ref()
                            .or(result.as_ref())
                            .filter(|s| !s.ty.is_aggregate())
                            .map(|s| s.index),
                        results: branch
                            .result
                            .as_ref()
                            .or(result.as_ref())
                            .filter(|s| s.ty.is_aggregate())
                            .map_or_else(Vec::new, |s| s.scalars().map(|s| s.index).collect()),
                        indirect: branch.indirect.as_ref().map(|s| s.index),
                        target: branch.indirect.is_none().then(|| ids[&branch.name]),
                        args: branch
                            .args
                            .iter()
                            .map(|slot| slot.as_ref().map(|slot| slot.index))
                            .collect(),
                        done: branch.done.index,
                    })
                    .collect(),
            },
            Op::ScopeFinish => Instruction::ScopeFinish,
            Op::TaskScopeEnter => Instruction::TaskScopeEnter,
            Op::TaskBranch {
                entry, captures, ..
            } => Instruction::TaskBranch {
                target: ids[entry],
                args: captures
                    .iter()
                    .map(|slot| slot.as_ref().map(|slot| slot.index))
                    .collect(),
            },
            Op::TaskJoin { .. } => Instruction::TaskJoin,
        };
        code.push(instruction);
        spans.push(*span);
    }
    if info.frame_slots != plan.slot_count {
        return Err(Error::new_with_code(
            crate::ErrorPhase::Compile,
            crate::ErrorCode::InvalidState,
            format!(
                "Interpreter frame layout mismatch: expected {}, got {}",
                info.frame_slots, plan.slot_count
            ),
        ));
    }
    Ok(BytecodeFunction {
        instance_hooks: plan.instance_hooks(&info.signature.ret),
        owner_result: plan
            .result
            .as_ref()
            .filter(|s| s.ty.is_owner())
            .and_then(|s| s.scalars().find(|s| s.ty.is_buffer()))
            .map(|s| s.index),
        name: "<anonymous>".into(),
        spans,
        info: info.clone(),
        code,
        result: plan
            .result
            .as_ref()
            .filter(|slot| !slot.ty.is_aggregate())
            .map(|slot| slot.index),
        aggregate_result: plan
            .result
            .as_ref()
            .filter(|slot| slot.ty.is_aggregate())
            .map(|slot| slot.scalars().map(|slot| slot.index).collect())
            .unwrap_or_default(),
    })
}

fn lower_constant(value: &Constant) -> ConstantValue {
    match value {
        Constant::NullPtr => ConstantValue::NullPtr,
        Constant::Float32(value) => ConstantValue::Float32(*value),
        Constant::Float64(value) => ConstantValue::Float64(*value),
        Constant::Int32(value) => ConstantValue::Int32(*value),
        Constant::Int64(value) => ConstantValue::Int64(*value),
        Constant::Int8(value) => ConstantValue::Int8(*value),
        Constant::Int16(value) => ConstantValue::Int16(*value),
        Constant::UInt8(value) => ConstantValue::UInt8(*value),
        Constant::UInt16(value) => ConstantValue::UInt16(*value),
        Constant::UInt32(value) => ConstantValue::UInt32(*value),
        Constant::Rune(value) => ConstantValue::Rune(*value),
        Constant::UInt64(value) => ConstantValue::UInt64(*value),
        Constant::Bool(value) => ConstantValue::Bool(*value),
        Constant::String(value) => ConstantValue::String(Rc::from(value.as_str())),
    }
}

pub(crate) fn function_signature<'a>(
    module: &'a BytecodeModule,
    name: &str,
) -> Option<&'a FunctionSignature> {
    module
        .functions
        .get(name)
        .filter(|function| !function.info.internal)
        .map(|function| &function.info.signature)
}

pub(crate) fn function_info<'a>(
    module: &'a BytecodeModule,
    name: &str,
) -> Option<&'a FunctionInfo> {
    module
        .functions
        .get(name)
        .filter(|function| !function.info.internal)
        .map(|function| &function.info)
}

/// Compiler-addressed request entries may be private imported functions.
pub(crate) fn request_info<'a>(module: &'a BytecodeModule, name: &str) -> Option<&'a FunctionInfo> {
    module.functions.get(name).map(|function| &function.info)
}

pub(crate) fn instance_hooks<'a>(module: &'a BytecodeModule, name: &str) -> &'a [DropHook] {
    &module.functions[name].instance_hooks
}

pub(crate) fn instance_info<'a>(
    module: &'a BytecodeModule,
    name: &str,
) -> Option<&'a FunctionInfo> {
    module
        .functions
        .get(name)
        .filter(|f| f.info.instance_entry)
        .map(|f| &f.info)
}

pub(crate) fn instance_names(module: &BytecodeModule) -> Vec<String> {
    module
        .functions
        .iter()
        .filter(|(_, f)| f.info.instance_entry)
        .map(|(name, _)| name.clone())
        .collect()
}

pub(crate) fn function_names(module: &BytecodeModule) -> Vec<String> {
    module
        .functions
        .iter()
        .filter(|(_, f)| !f.info.internal)
        .map(|(name, _)| name.clone())
        .collect()
}

#[derive(Clone)]
pub(crate) enum Value {
    DynTable(Rc<DynMethodTable>),
    Function(usize),
    Unit,
    Float32(f32),
    Float64(f64),
    Int8(i8),
    Int16(i16),
    Int32(i32),
    Int64(i64),
    UInt8(u8),
    UInt16(u16),
    UInt32(u32),
    Rune(u32),
    UInt64(u64),
    Bool(bool),
    String(Rc<str>),
    HostText(*const u8),
    Named(u64),
    Reference(*mut u8),
    Ptr(*mut u8),
}

impl Value {
    fn from_instance_bits(ty: &Type, bits: u64) -> Result<Self, Error> {
        use crate::core::instance::InstanceValue as V;
        if matches!(ty, Type::Reference(_)) {
            return Ok(Self::Reference(bits as usize as *mut u8));
        }
        Ok(
            match V::from_bits(ty, bits).ok_or_else(|| abi_error("Invalid instance scalar"))? {
                V::Unit => Self::Unit,
                V::Bool(v) => Self::Bool(v),
                V::Rune(v) => Self::Rune(u32::from(v)),
                V::Int8(v) => Self::Int8(v),
                V::Int16(v) => Self::Int16(v),
                V::Int32(v) => Self::Int32(v),
                V::Int64(v) => Self::Int64(v),
                V::UInt8(v) => Self::UInt8(v),
                V::UInt16(v) => Self::UInt16(v),
                V::UInt32(v) => Self::UInt32(v),
                V::UInt64(v) => Self::UInt64(v),
                V::Float32(v) => Self::Float32(v),
                V::Float64(v) => Self::Float64(v),
            },
        )
    }

    fn instance_bits(&self) -> Result<u64, Error> {
        if let Self::Reference(pointer) = self {
            return Ok(*pointer as usize as u64);
        }
        self.pack_scalar()
            .ok_or_else(|| abi_error("Instance result is not a plain scalar"))
    }

    /// Lossless CPS bits for plain scalar values. References, owners, callables
    /// and opaque capabilities use separate checked boundary adapters.
    pub(crate) fn pack_scalar(&self) -> Option<u64> {
        Some(match self {
            Value::Unit => 0,
            Value::Bool(value) => u64::from(*value),
            Value::Float32(value) => u64::from(value.to_bits()),
            Value::Float64(value) => value.to_bits(),
            Value::Int8(value) => u64::from(*value as u8),
            Value::Int16(value) => u64::from(*value as u16),
            Value::Int32(value) => *value as u32 as u64,
            Value::Int64(value) => *value as u64,
            Value::UInt8(value) => u64::from(*value),
            Value::UInt16(value) => u64::from(*value),
            Value::UInt32(value) => u64::from(*value),
            Value::Rune(value) => u64::from(*value),
            Value::UInt64(value) => *value,
            _ => return None,
        })
    }
}

fn runtime_error(code: ErrorCode, message: impl Into<String>) -> Error {
    Error::new_with_code(ErrorPhase::Runtime, code, message)
}

fn abi_error(message: impl Into<String>) -> Error {
    Error::new_with_code(ErrorPhase::Abi, ErrorCode::AbiError, message)
}
