use std::sync::{Condvar, Mutex, OnceLock};

use cranelift_codegen::isa::TargetIsa;
use cranelift_codegen::Context;
use cranelift_module::{FuncId, ModuleReloc};

use super::{CraneliftBackend, ModuleLifecycle};
use crate::codegen::debug::PendingDebugRecord;
use crate::codegen::helpers::codegen_error;
use crate::diagnostic::CodegenError;
use crate::mir::MirFunction;

pub(super) struct PendingMachineFunction {
    func_id: FuncId,
    context: Context,
    debug: Option<PendingDebugRecord>,
}

struct CompileSlots {
    remaining: Mutex<usize>,
    waiters: Condvar,
}

impl CompileSlots {
    fn acquire(&self) {
        let mut remaining = self.remaining.lock().expect("codegen compile slots");
        while *remaining == 0 {
            remaining = self.waiters.wait(remaining).expect("codegen compile slots");
        }
        *remaining -= 1;
    }

    fn release(&self) {
        let mut remaining = self.remaining.lock().expect("codegen compile slots");
        *remaining += 1;
        self.waiters.notify_one();
    }
}

fn compile_slots() -> &'static CompileSlots {
    static SLOTS: OnceLock<CompileSlots> = OnceLock::new();
    SLOTS.get_or_init(|| CompileSlots {
        remaining: Mutex::new(default_codegen_jobs()),
        waiters: Condvar::new(),
    })
}

/// `JOKY_CODEGEN_JOBS` caps how many Cranelift `compile()` calls run at once
/// in this process. The default is the host's available parallelism so a
/// single `joky build` can use the machine; CLI tests set the variable to 2.
pub(crate) fn default_codegen_jobs() -> usize {
    if let Ok(value) = std::env::var("JOKY_CODEGEN_JOBS") {
        match value.trim().parse::<usize>() {
            Ok(jobs) if jobs > 0 => return jobs,
            _ => eprintln!("ignoring invalid JOKY_CODEGEN_JOBS '{value}'"),
        }
    }
    std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(1)
}

pub(crate) fn codegen_job_count(pending: usize) -> usize {
    codegen_job_count_with(pending, std::env::var("JOKY_CODEGEN_JOBS").ok().as_deref())
}

fn codegen_job_count_with(pending: usize, env: Option<&str>) -> usize {
    let requested = match env {
        Some(value) => match value.trim().parse::<usize>() {
            Ok(jobs) if jobs > 0 => jobs,
            _ => {
                eprintln!("ignoring invalid JOKY_CODEGEN_JOBS '{value}'");
                default_codegen_jobs()
            }
        },
        None => default_codegen_jobs(),
    };
    requested.min(pending.max(1)).max(1)
}

impl<M: ModuleLifecycle> CraneliftBackend<M> {
    pub(in crate::codegen) fn enqueue_machine_function(
        &mut self,
        func_id: FuncId,
        context: Context,
        function: &MirFunction,
        resume: Option<usize>,
    ) {
        let debug = self
            .debug_info
            .as_ref()
            .and_then(|debug| debug.pending_record(function, resume));
        self.pending_machine_functions.push(PendingMachineFunction {
            func_id,
            context,
            debug,
        });
    }

    pub(super) fn flush_pending_machine_functions(&mut self) -> Result<(), CodegenError> {
        let mut pending = std::mem::take(&mut self.pending_machine_functions);
        if pending.is_empty() {
            return Ok(());
        }
        compile_pending_machine_functions(&mut pending, self.module.isa())?;
        for item in pending {
            self.define_compiled_machine_function(item)?;
        }
        Ok(())
    }

    fn define_compiled_machine_function(
        &mut self,
        item: PendingMachineFunction,
    ) -> Result<(), CodegenError> {
        let PendingMachineFunction {
            func_id,
            context,
            debug,
        } = item;
        let compiled = context
            .compiled_code()
            .ok_or_else(|| CodegenError::RuntimeError {
                message: "compiled function is missing machine code".into(),
            })?;
        let alignment = u64::from(compiled.buffer.alignment);
        let relocs = compiled
            .buffer
            .relocs()
            .iter()
            .map(|reloc| ModuleReloc::from_mach_reloc(reloc, &context.func, func_id))
            .collect::<Vec<_>>();
        let bytes = compiled.buffer.data().to_vec();
        if let (Some(debug_info), Some(pending_debug)) = (self.debug_info.as_mut(), debug) {
            debug_info.record_compiled(func_id, pending_debug, &context);
        }
        self.module
            .define_function_bytes(func_id, alignment, &bytes, &relocs)
            .map_err(codegen_error)?;
        Ok(())
    }
}

fn compile_pending_machine_functions(
    pending: &mut [PendingMachineFunction],
    isa: &dyn TargetIsa,
) -> Result<(), CodegenError> {
    let jobs = codegen_job_count(pending.len());
    if jobs <= 1 {
        for item in pending.iter_mut() {
            compile_machine_context(item, isa)?;
        }
        return Ok(());
    }
    let chunk_size = pending.len().div_ceil(jobs);
    let errors = Mutex::new(None);
    std::thread::scope(|scope| {
        for chunk in pending.chunks_mut(chunk_size) {
            scope.spawn(|| {
                for item in chunk {
                    if errors.lock().expect("codegen compile errors").is_some() {
                        return;
                    }
                    if let Err(error) = compile_machine_context(item, isa) {
                        *errors.lock().expect("codegen compile errors") = Some(error);
                        return;
                    }
                }
            });
        }
    });
    match errors.into_inner().expect("codegen compile errors") {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn compile_machine_context(
    item: &mut PendingMachineFunction,
    isa: &dyn TargetIsa,
) -> Result<(), CodegenError> {
    let slots = compile_slots();
    slots.acquire();
    let result = item
        .context
        .compile(isa, &mut Default::default())
        .map(|_| ())
        .map_err(|error| codegen_error(error.inner));
    slots.release();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn codegen_job_count_defaults_and_clamps() {
        assert_eq!(codegen_job_count_with(1, None), 1);
        assert_eq!(codegen_job_count_with(8, Some("4")), 4);
        assert_eq!(codegen_job_count_with(2, Some("8")), 2);
        assert_eq!(codegen_job_count_with(3, Some("1")), 1);
        let fallback = codegen_job_count_with(8, None);
        assert_eq!(codegen_job_count_with(8, Some("0")), fallback);
        assert_eq!(codegen_job_count_with(8, Some("nope")), fallback);
    }

    #[test]
    fn compile_slots_allow_one_job_at_a_time_when_configured() {
        let slots = CompileSlots {
            remaining: Mutex::new(1),
            waiters: Condvar::new(),
        };
        let concurrent = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    slots.acquire();
                    let now = concurrent.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    std::thread::yield_now();
                    concurrent.fetch_sub(1, Ordering::SeqCst);
                    slots.release();
                });
            }
        });
        assert_eq!(peak.load(Ordering::SeqCst), 1);
    }
}
