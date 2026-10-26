//! Freeze an admitted entry before moving work to a worker. This is preparation
//! infrastructure used by explicit parallel execution; Local semantics stay unchanged.
#[cfg(test)]
pub(crate) mod array_tests;
mod arrays;
#[cfg(test)]
pub(crate) mod fixed_array_tests;
#[cfg(test)]
pub(crate) mod list_tests;
mod lists;
#[cfg(test)]
pub(crate) mod map_tests;
mod maps;
pub use maps::{TaskMap, TaskSet};
#[cfg(test)]
mod control_tests;
#[cfg(test)]
mod domain_tests;
#[cfg(test)]
mod function_tests;
mod places;
#[cfg(test)]
mod tcp_tests;
mod value;
use super::{InstanceValue, InterpreterLimits};
use crate::{
    backends::{interp, jit},
    runtime::{
        cps::CpsEntry,
        parallel::{
            packet::{Packet, Shape},
            pool, Guard, Host, Lease, Storage, TaskBudget,
        },
    },
    Error, ErrorCode, ErrorPhase, FunctionSignature, Type,
};
use std::{any::Any, sync::Arc, task::Poll};

fn invalid(message: &str) -> Error {
    Error::new_with_code(ErrorPhase::Abi, ErrorCode::InvalidCall, message)
}

/// Pointer-free owning inputs and results of a frozen worker entry.
#[derive(Clone, Debug, PartialEq)]
pub enum TaskValue {
    Scalar(InstanceValue),
    String(String),
    Tuple(Vec<TaskValue>),
    /// FixedArray identity and exactly its declared number of Copy elements.
    FixedArray {
        ty: Type,
        elements: Vec<TaskValue>,
    },
    /// Standard Array identity and initialized elements in source order.
    Array {
        ty: Type,
        elements: Vec<TaskValue>,
    },
    /// Standard List identity and elements in logical sequence order.
    List {
        ty: Type,
        elements: Vec<TaskValue>,
    },
    /// A read-only Map snapshot produced by a checked task.
    Map(TaskMap),
    /// A read-only Set snapshot produced by a checked task.
    Set(TaskSet),
    /// Fields are in declaration order; `ty` comes from the frozen signature.
    Record {
        ty: Type,
        fields: Vec<TaskValue>,
    },
    /// Variant indices are in declaration order. Nullary variants have no payload.
    Enum {
        ty: Type,
        variant: usize,
        payload: Option<Box<TaskValue>>,
    },
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
    input: Vec<Packet>,
    root: Storage,
    limits: InterpreterLimits,
    _packet: Lease,
}
/// A started task is confined to its worker, including cancellation and Drop.
pub struct ParallelExecution {
    task: Option<Box<dyn pool::Job>>,
    root: Storage,
    code: Code,
    result_type: Type,
    packet: Option<Packet>,
    finished: bool,
    failure: Option<Error>,
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
        self.submit_to(&executor.submitter())
    }
    /// Submit from any host thread through the thread-safe submission handle.
    pub fn submit_to(
        self,
        submitter: &crate::runtime::parallel::ParallelSubmitter,
    ) -> Result<ParallelTask, Error> {
        let root = self.root.clone();
        let code = Arc::new(self.code.clone());
        let handle = submitter.publish(
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
        input: Vec<TaskValue>,
        root: Storage,
        limits: InterpreterLimits,
    ) -> Result<Self, Error> {
        let info = match &code {
            Code::Native(e) => &e.info,
            Code::Interpreted(e) => &e.info,
        };
        let sig = &info.signature;
        // Internal child packets can carry TCP owners and proved language callables.
        // The public TaskValue boundary stays pointer-free.
        use crate::core::types::walk::{TypeEdges, TypeWalk};
        if sig
            .params
            .iter()
            .chain(std::iter::once(&sig.ret))
            .any(|ty| {
                TypeWalk::new(ty, TypeEdges::Fields).any(|ty| {
                    matches!(ty, Type::Resource(_) | Type::Function(_))
                        || ty.is_domain_capability()
                        || matches!(ty, Type::Record(r) if matches!(r.name.as_str(), "joky/domain.DirectoryHandle" | "joky/domain.TimerHandle" | "joky/outbox.Handle"))
                })
            })
        {
            return Err(invalid(
                "Host task values cannot contain resources, control handles or callables",
            ));
        }
        Shape::of(&sig.ret)?;
        for ty in &sig.params {
            Shape::of(ty)?;
        }
        if !info.suspends
            || sig.params.len() != input.len()
            || !input
                .iter()
                .zip(&sig.params)
                .all(|(value, ty)| value.matches(ty))
        {
            return Err(invalid(
                "Parallel task requires a matching @suspends signature and exact value types",
            ));
        }
        let bytes = input
            .iter()
            .try_fold(std::mem::size_of::<Self>() + name.len(), |n, value| {
                n.checked_add(value.bytes())
            })
            .ok_or_else(|| invalid("Parallel capture size overflow"))?;
        let temporary = root.accounts().reserve(bytes)?;
        let packet = root.accounts().reserve(
            std::mem::size_of::<Self>() + name.len() + input.len() * std::mem::size_of::<Packet>(),
        )?;
        let _guard = Guard::enter(&root);
        let mut arena = crate::runtime::arena::Arena::with_storage(root.clone());
        let input = input
            .into_iter()
            .zip(&sig.params)
            .map(|(value, ty)| value.packet(ty, &mut arena))
            .collect::<Result<_, _>>()?;
        drop(temporary);
        Ok(Self {
            code,
            name: name.into(),
            input,
            root,
            limits,
            _packet: packet,
        })
    }
    /// Build all mutable execution state on the receiving thread, without polling.
    pub fn start(self) -> Result<ParallelExecution, Error> {
        let _guard = Guard::enter(&self.root);
        let (task, result_type) = match &self.code {
            Code::Native(code) => (
                crate::runtime::cps::parallel::start(
                    code.entry,
                    code.info.frame_slots,
                    code.info.parameter_slots(),
                    Shape::of(&code.info.signature.ret)?,
                    self.input,
                )?,
                code.info.signature.ret.clone(),
            ),
            Code::Interpreted(code) => (
                interp::start_parallel(code, &self.name, self.input, self.limits)?,
                code.info.signature.ret.clone(),
            ),
        };
        Ok(ParallelExecution {
            task: Some(task),
            root: self.root.clone(),
            code: self.code,
            result_type,
            packet: None,
            finished: false,
            failure: None,
        })
    }
}
impl ParallelExecution {
    /// Return true after completion. Pending work stays on this worker.
    pub fn resume(&mut self) -> Result<bool, Error> {
        let _guard = Guard::enter(&self.root);
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        if self.finished {
            return Ok(true);
        }
        let outcome = self
            .task
            .as_mut()
            .ok_or_else(|| invalid("Parallel result was already taken"))?
            .poll();
        match outcome {
            Poll::Pending => Ok(false),
            Poll::Ready(result) => {
                self.finished = true;
                let result = result.and_then(|v| {
                    v.downcast::<Packet>()
                        .map(|v| *v)
                        .map_err(|_| invalid("Invalid parallel root result"))
                });
                // Teardown stays in the root storage context, including failures.
                let cleanup = crate::runtime::error::checked(|| drop(self.task.take()));
                let result = match (result, cleanup) {
                    (Ok(packet), Ok(())) => Ok(packet),
                    (Err(error), Err(cleanup)) if error.code() == ErrorCode::Cancelled => {
                        Err(cleanup)
                    }
                    (Err(error), _) | (_, Err(error)) => Err(error),
                };
                match result {
                    Ok(packet) => {
                        self.packet = Some(packet);
                        Ok(true)
                    }
                    Err(error) => {
                        let sources = match &self.code {
                            Code::Native(c) => &c.sources,
                            Code::Interpreted(c) => &c.sources,
                        };
                        let error = error.with_sources(sources);
                        self.failure = Some(error.clone());
                        Err(error)
                    }
                }
            }
        }
    }
    pub fn take_result(&mut self) -> Result<ParallelOutput, Error> {
        let _guard = Guard::enter(&self.root);
        let packet = self
            .packet
            .as_ref()
            .ok_or_else(|| invalid("Parallel task is pending, failed or already collected"))?;
        let output = value::output(packet, &self.result_type, &self.root)?;
        self.packet.take();
        Ok(output)
    }
}
impl Drop for ParallelExecution {
    fn drop(&mut self) {
        let _guard = Guard::enter(&self.root);
        self.task.take();
        self.packet.take();
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
        code: crate::runtime::parallel::HostCode::Cps { entry, slots },
        _owner: owner,
    }
}

#[cfg(test)]
pub(crate) mod aggregates;
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
    pub fn wait(&self) -> Result<(), Error> {
        self.handle.wait()
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
