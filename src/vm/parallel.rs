//! Freeze an admitted entry before moving work to a worker. This is preparation
//! infrastructure, not the worker-pool scheduler or a change to Local semantics.
use super::{InstanceValue, InterpreterLimits};
use crate::{
    backends::{interp, jit},
    runtime::{
        cps::{CpsEntry, CpsFrame},
        parallel::{Guard, Host, Lease, OwnedBuffer, Storage, TaskBudget},
        suspension::{Continuation, StringContinuation},
    },
    Error, ErrorCode, ErrorPhase, FunctionSignature, Type,
};
use std::{any::Any, rc::Rc, sync::Arc};

fn invalid(message: &str) -> Error {
    Error::new_with_code(ErrorPhase::Abi, ErrorCode::InvalidCall, message)
}

/// Pointer-free owning inputs and results of a frozen worker entry.
#[derive(Debug, PartialEq)]
pub enum TaskValue {
    Scalar(InstanceValue),
    String(String),
}
impl TaskValue {
    fn ty(&self) -> Type {
        match self {
            Self::Scalar(v) => v.value_type(),
            Self::String(_) => Type::String,
        }
    }
    fn bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + match self {
                Self::String(s) => s.capacity(),
                _ => 0,
            }
    }
}
#[derive(Clone)]
pub(crate) enum Code {
    Native(Arc<jit::FrozenEntry>),
    Interpreted(Arc<interp::FrozenEntry>),
}
/// Sendable preparation. It owns no frame, local cleanup callback or VM handle.
pub struct PreparedParallelTask {
    code: Code,
    name: String,
    input: Vec<TaskValue>,
    text_input: Option<OwnedBuffer>,
    root: Storage,
    limits: InterpreterLimits,
    _packet: Lease,
}
/// A started task is confined to its worker, including cancellation and Drop.
pub struct ParallelExecution {
    task: Option<Running>,
    root: Storage,
    result_type: Type,
}
enum Running {
    Scalar(Continuation),
    Text(StringContinuation),
}
/// The result retains its budget charge until consumed or dropped.
pub struct ParallelOutput {
    value: TaskValue,
    _charge: Lease,
}
impl ParallelOutput {
    pub fn value(&self) -> &TaskValue {
        &self.value
    }
    pub fn into_value(self) -> TaskValue {
        self.value
    }
}
impl PreparedParallelTask {
    /// Submit a checked root. Its descendants use the same fixed-worker executor.
    pub fn submit(
        self,
        executor: &crate::runtime::parallel::ParallelExecutor,
    ) -> Result<ParallelTask, Error> {
        let root = self.root.clone();
        let code = Arc::new(self.code.clone());
        let handle = executor.submit(
            root,
            code,
            Box::new(move || Ok(Box::new(RootJob(self.start()?)))),
        )?;
        Ok(ParallelTask { handle })
    }

    pub(crate) fn native(
        entry: jit::FrozenEntry,
        name: &str,
        input: Vec<TaskValue>,
        budgets: Vec<TaskBudget>,
        limits: InterpreterLimits,
    ) -> Result<Self, Error> {
        Self::new(Code::Native(Arc::new(entry)), name, input, budgets, limits)
    }
    pub(super) fn interpreted(
        entry: interp::FrozenEntry,
        name: &str,
        input: Vec<TaskValue>,
        budgets: Vec<TaskBudget>,
        limits: InterpreterLimits,
    ) -> Result<Self, Error> {
        Self::new(
            Code::Interpreted(Arc::new(entry)),
            name,
            input,
            budgets,
            limits,
        )
    }
    fn new(
        code: Code,
        name: &str,
        input: Vec<TaskValue>,
        mut budgets: Vec<TaskBudget>,
        limits: InterpreterLimits,
    ) -> Result<Self, Error> {
        budgets.push(TaskBudget::new(crate::runtime::arena::MAX_BYTES));
        Self::in_root(code, name, input, Storage::new(budgets)?, limits)
    }
    fn in_root(
        code: Code,
        name: &str,
        mut input: Vec<TaskValue>,
        root: Storage,
        limits: InterpreterLimits,
    ) -> Result<Self, Error> {
        let info = match &code {
            Code::Native(e) => &e.info,
            Code::Interpreted(e) => &e.info,
        };
        let sig = &info.signature;
        if !info.suspends || sig.params != input.iter().map(TaskValue::ty).collect::<Vec<_>>() {
            return Err(invalid(
                "Parallel task requires a matching @suspends signature",
            ));
        }
        if sig.has_text() {
            if sig.ret != Type::String || !(sig.params.is_empty() || sig.params == [Type::String]) {
                return Err(invalid(
                    "Parallel text root supports () or (String) -> String",
                ));
            }
        } else if !crate::core::instance::scalar(&sig.ret)
            || !sig.params.iter().all(crate::core::instance::scalar)
        {
            return Err(invalid(
                "Parallel root packet requires scalar or String values",
            ));
        }
        let bytes = input
            .iter()
            .try_fold(std::mem::size_of::<Self>() + name.len(), |n, value| {
                n.checked_add(value.bytes())
            })
            .ok_or_else(|| invalid("Parallel capture size overflow"))?;
        let packet = root.accounts().reserve(bytes)?;
        let mut text_input = None;
        if let Some(TaskValue::String(text)) = input.first_mut() {
            let mut arena = crate::runtime::arena::Arena::with_storage(root.clone());
            let owner = crate::runtime::buffer::Buffer::copy_bytes(&mut arena, text.as_bytes())?;
            text_input = Some(match unsafe { OwnedBuffer::new(&arena, owner) } {
                Ok(packet) => packet,
                Err(error) => {
                    unsafe { crate::runtime::buffer::Buffer::release(&mut arena, owner.as_ptr()) }?;
                    return Err(error);
                }
            });
            *text = String::new();
        }
        Ok(Self {
            code,
            name: name.into(),
            input,
            text_input,
            root,
            limits,
            _packet: packet,
        })
    }
    /// Build all mutable execution state on the receiving thread, without polling.
    pub fn start(mut self) -> Result<ParallelExecution, Error> {
        let _guard = Guard::enter(&self.root);
        let result_type = match &self.code {
            Code::Native(e) => e.info.signature.ret.clone(),
            Code::Interpreted(e) => e.info.signature.ret.clone(),
        };
        let task = match &self.code {
            Code::Native(code) => {
                let mut frame = CpsFrame::try_new(code.info.frame_slots)?;
                frame.root_text = result_type == Type::String;
                for (index, value) in self.input.iter().enumerate() {
                    match value {
                        TaskValue::Scalar(v) => frame.slots[index] = v.bits(),
                        TaskValue::String(_) => unsafe {
                            let arena = crate::runtime::cps::arena(&mut frame);
                            let owner = self
                                .text_input
                                .as_mut()
                                .expect("prepared text")
                                .take(&*arena)?;
                            frame.slots[index] = owner as usize as u64;
                            let cleanup = crate::runtime::cps::cleanup(&mut frame);
                            (*cleanup).entries.insert(
                                index,
                                crate::runtime::cleanup::Entry {
                                    keep_alive: None,
                                    storage_size: 0,
                                    storage: None,
                                    owners: vec![Some(owner)],
                                    hooks: vec![],
                                },
                            );
                        },
                    }
                }
                unsafe { Continuation::new(code.entry, frame, code.version, Rc::new(code.clone())) }
                    .with_sources(code.sources.clone())
            }
            Code::Interpreted(code) => {
                let module = code.instantiate()?;
                let task = if result_type == Type::String {
                    interp::start_owned_text(
                        module.clone(),
                        &self.name,
                        self.text_input.as_mut(),
                        self.limits,
                    )?
                } else {
                    let args: Vec<_> = self
                        .input
                        .iter()
                        .map(|value| match value {
                            TaskValue::Scalar(v) => *v,
                            _ => unreachable!(),
                        })
                        .collect();
                    interp::prepare_scalar(module.clone(), &self.name, &args, self.limits)?
                };
                Continuation::new_interpreted(task, code.version, module)
                    .with_sources(code.sources.clone())
            }
        };
        Ok(ParallelExecution {
            task: Some(if result_type == Type::String {
                Running::Text(StringContinuation::new(task))
            } else {
                Running::Scalar(task)
            }),
            root: self.root.clone(),
            result_type,
        })
    }
}
impl ParallelExecution {
    /// Return true after completion. Pending work stays on this worker.
    pub fn resume(&mut self) -> Result<bool, Error> {
        let _guard = Guard::enter(&self.root);
        match self
            .task
            .as_mut()
            .ok_or_else(|| invalid("Parallel result was already taken"))?
        {
            Running::Scalar(task) => {
                task.resume()?;
                Ok(task.is_finished())
            }
            Running::Text(task) => {
                task.resume()?;
                Ok(task.is_finished())
            }
        }
    }
    pub fn take_result(&mut self) -> Result<ParallelOutput, Error> {
        let _guard = Guard::enter(&self.root);
        let task = self
            .task
            .as_mut()
            .ok_or_else(|| invalid("Parallel result was already taken"))?;
        let mut text_charge = None;
        let value = match task {
            Running::Scalar(task) => {
                let bits = task
                    .cps_result()
                    .ok_or_else(|| invalid("Parallel task has not completed successfully"))?;
                TaskValue::Scalar(
                    InstanceValue::from_bits(&self.result_type, bits)
                        .ok_or_else(|| invalid("Invalid parallel scalar result"))?,
                )
            }
            Running::Text(task) => {
                text_charge = task.take_parallel_result_charge();
                TaskValue::String(
                    task.take_result()
                        .ok_or_else(|| invalid("Parallel task has not completed successfully"))?,
                )
            }
        };
        let charge = match text_charge {
            Some(charge) => charge,
            None => self.root.accounts().reserve(value.bytes())?,
        };
        self.task.take();
        Ok(ParallelOutput {
            value,
            _charge: charge,
        })
    }
}
impl Drop for ParallelExecution {
    fn drop(&mut self) {
        let _guard = Guard::enter(&self.root);
        self.task.take();
    }
}

/// The registration retains this descriptor through the ordinary local owner slot.
pub(super) fn host(
    name: &str,
    signature: FunctionSignature,
    entry: CpsEntry,
    slots: usize,
    owner: Arc<dyn Any + Send + Sync>,
) -> Host {
    Host {
        name: name.into(),
        signature,
        entry,
        slots,
        _owner: owner,
    }
}

#[cfg(test)]
mod tests;

/// Thread-safe observation and cancellation; dropping the handle requests stop.
pub struct ParallelTask {
    handle: crate::runtime::parallel::pool::Handle,
}
impl ParallelTask {
    pub fn request_cancel(&self) {
        self.handle.cancel();
    }
    pub fn is_finished(&self) -> bool {
        self.handle.cleaned()
    }
    pub fn wait(&self) {
        self.handle.wait();
    }
    pub fn take_result(&self) -> Result<ParallelOutput, Error> {
        let result = self
            .handle
            .take()
            .ok_or_else(|| invalid("Parallel task is pending or already collected"))??;
        result
            .downcast::<ParallelOutput>()
            .map(|value| *value)
            .map_err(|_| invalid("Invalid parallel root result"))
    }
}
impl Drop for ParallelTask {
    fn drop(&mut self) {
        self.handle.cancel();
    }
}
struct RootJob(ParallelExecution);
impl crate::runtime::parallel::pool::Job for RootJob {
    fn poll(&mut self) -> std::task::Poll<Result<crate::runtime::parallel::pool::Output, Error>> {
        match self.0.resume() {
            Ok(false) => std::task::Poll::Pending,
            Ok(true) => std::task::Poll::Ready(self.0.take_result().map(|v| Box::new(v) as _)),
            Err(e) => std::task::Poll::Ready(Err(e)),
        }
    }
}
