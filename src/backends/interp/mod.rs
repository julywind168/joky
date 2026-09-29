//! Register bytecode interpreter for platforms where executable memory is not
//! available. The bytecode is produced from the same typed plans as JIT/AOT.
use crate::{
    core::{
        integer::{Comparison, IntegerType},
        ir::{Constant, FunctionInfo, FunctionPlan, ModulePlan, Op},
    },
    parser::ast::{BinOp, Module, UnaryOp},
    runtime::{bindings::HostBinding, suspension::ModuleId},
    Error, ErrorCode, ErrorPhase, FunctionSignature, Span, TypeRegistry,
};
use std::{
    collections::{BTreeSet, HashMap},
    rc::Rc,
};

#[cfg(test)]
mod diagnostics;
mod execute;
mod host;
mod memory;
mod optimize;

pub(crate) use execute::{call_sync, start};
#[cfg(test)]
use execute::{Execution, Frame, Poll};
use host::BoundHost;

enum ConstantValue {
    EmptyReference,
    Int32(i32),
    Int64(i64),
    UInt32(u32),
    UInt64(u64),
    Bool(bool),
    String(Rc<str>),
}

enum Instruction {
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
        target: IntegerType,
        operand: usize,
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
        branches: Vec<BytecodeBranch>,
    },
}

struct AggregateCall {
    target: usize,
    args: Vec<Option<usize>>,
    results: Vec<usize>,
}

struct BytecodeBranch {
    target: usize,
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
}

pub(crate) struct BytecodeModule {
    pub(crate) version: ModuleId,
    sources: std::sync::Arc<crate::SourceMap>,
    functions: HashMap<String, Rc<BytecodeFunction>>,
    callees: Vec<Callee>,
}

enum Callee {
    User(Rc<BytecodeFunction>),
    Host(Rc<BoundHost>),
}

pub(crate) fn compile_with_types(
    ast: &Module,
    imports: &HashMap<String, usize>,
    hosts: HashMap<String, HostBinding>,
    version: ModuleId,
    types: &TypeRegistry,
) -> Result<BytecodeModule, Error> {
    let plan = ModulePlan::with_import_layouts_and_types(ast, imports, types)?;
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
                Op::Call { name, .. } => vec![name],
                Op::Scope { branches, .. } => branches.iter().map(|b| b.name.as_str()).collect(),
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
    for name in &names {
        if let Some(body) = plan.bodies.get(name) {
            let mut function = lower_function(&plan.functions[name], body, &ids)?;
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
        sources: ast.sources.clone(),
        version,
        functions,
        callees,
    })
}

fn lower_function(
    info: &FunctionInfo,
    plan: &FunctionPlan,
    ids: &HashMap<String, usize>,
) -> Result<BytecodeFunction, Error> {
    let mut function = lower_unoptimized(info, plan, ids)?;
    // The current optimizer reasons about values, not addressable aliasing.
    if !info.uses_arena && !plan.has_references() {
        optimize::optimize(&mut function);
        optimize::compact_slots(&mut function);
    }
    Ok(function)
}

fn lower_unoptimized(
    info: &FunctionInfo,
    plan: &FunctionPlan,
    ids: &HashMap<String, usize>,
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
            Op::Graph(_) => unreachable!("construction operations are erased after checking"),
            Op::EndLocal(_) => unreachable!("local lifetime markers are erased after checking"),
            Op::Alloc { .. }
            | Op::Local { .. }
            | Op::ReadMemory { .. }
            | Op::WriteMemory { .. } => Instruction::Memory(Box::new(memory::lower(op)?)),
            Op::Copy(dst, src) | Op::Reference { dst, src, .. } | Op::Reserve { dst, src } => {
                Instruction::Copy {
                    dst: dst.index,
                    src: src.index,
                }
            }
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
                value: if dst.ty.reference_type().is_some() {
                    ConstantValue::EmptyReference
                } else {
                    lower_constant(value)
                },
            },
            Op::FunctionRef(_, _) => {
                // The callee scan in compile_with_types rejects every function
                // reference in the module before any lowering runs.
                unreachable!("native function reference survived the callee scan")
            }
            Op::Binary(dst, op, lhs, rhs) => Instruction::Binary {
                dst: dst.index,
                op: op.clone(),
                lhs: lhs.index,
                rhs: rhs.index,
            },
            Op::Convert(dst, src) => Instruction::Convert {
                dst: dst.index,
                target: dst.ty.integer().unwrap(),
                operand: src.index,
            },
            Op::Unary(dst, op, operand) => Instruction::Unary {
                dst: dst.index,
                op: op.clone(),
                operand: operand.index,
            },
            Op::Call {
                name, args, result, ..
            } => {
                let args = args
                    .iter()
                    .map(|slot| slot.as_ref().map(|slot| slot.index))
                    .collect();
                if let Some(result) = result.as_ref().filter(|slot| slot.ty.is_aggregate()) {
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
            Op::Scope { branches, .. } => Instruction::Scope {
                branches: branches
                    .iter()
                    .map(|branch| BytecodeBranch {
                        target: ids[&branch.name],
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
        Constant::Int32(value) => ConstantValue::Int32(*value),
        Constant::Int64(value) => ConstantValue::Int64(*value),
        Constant::UInt32(value) => ConstantValue::UInt32(*value),
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
    Unit,
    Int32(i32),
    Int64(i64),
    UInt32(u32),
    UInt64(u64),
    Bool(bool),
    String(Rc<str>),
    Named(u64),
    Reference(*mut u8),
}

impl Value {
    /// 64-bit slot representation for the scalar widths a task result or CPS
    /// frame carries: the four integers plus Unit. Bool, String and Named
    /// have slot conventions specific to their call boundaries and are
    /// rejected here.
    pub(crate) fn pack_scalar(&self) -> Option<u64> {
        Some(match self {
            Value::Unit => 0,
            Value::Int32(value) => *value as u32 as u64,
            Value::Int64(value) => *value as u64,
            Value::UInt32(value) => u64::from(*value),
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
