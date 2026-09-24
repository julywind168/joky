//! Owned child execution with concurrent pipe transport and scope cleanup.
//!
//! Start validates ABI shapes and copies borrowed values into Rust-owned
//! configuration. No generated pointers survive admission. The supervisor owns
//! the child, pipe workers and a cleanup lease until it has killed/reaped and
//! joined them. Cancellation only sets a persistent flag; late results are
//! released if completion refuses transfer. No cleanup calls generated code.
use super::continuation::Continuation;
use super::managed::{jk_drop, valid_header, RuntimeValueKind};
use super::provider::{ProviderOperationEntry, ProviderScope, ProviderStart};
use super::scope::current_or_default;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

mod protocol;
mod runner;

#[derive(Default)]
struct CommandSpec {
    program: String,
    args: Vec<String>,
    cwd: Option<String>,
    replace: bool,
    env: Vec<(String, Option<String>)>,
}
const WORD: usize = std::mem::size_of::<usize>();
type CaptureInput = Option<(Vec<u8>, usize)>;
type Key = (usize, u64);
fn requests() -> &'static Mutex<HashMap<Key, Arc<AtomicBool>>> {
    static REQUESTS: OnceLock<Mutex<HashMap<Key, Arc<AtomicBool>>>> = OnceLock::new();
    REQUESTS.get_or_init(Mutex::default)
}
struct Registration(Key);
impl Drop for Registration {
    fn drop(&mut self) {
        requests().lock().expect("process requests").remove(&self.0);
    }
}

pub(crate) fn register_operations(entries: &[ProviderOperationEntry]) -> Option<ProviderScope> {
    super::provider::register_named(
        &[
            ("status", status_start as ProviderStart),
            ("output", output_start),
            ("pipeline_status", pipeline_status_start),
            ("pipeline_output", pipeline_output_start),
        ],
        cancel,
        entries,
    )
}
unsafe extern "C" fn cancel(h: *mut Continuation, op: u64) -> u8 {
    if let Some(flag) = requests()
        .lock()
        .expect("process requests")
        .get(&(h as usize, op))
    {
        flag.store(true, Ordering::Release);
    }
    1
}
unsafe extern "C" fn status_start(
    h: *mut Continuation,
    op: u64,
    a: *const u8,
    n: usize,
    _: *mut u8,
    r: usize,
) -> u8 {
    start_request(h, op, a, n, r, false, false)
}
unsafe extern "C" fn output_start(
    h: *mut Continuation,
    op: u64,
    a: *const u8,
    n: usize,
    _: *mut u8,
    r: usize,
) -> u8 {
    start_request(h, op, a, n, r, true, false)
}
unsafe extern "C" fn pipeline_status_start(
    h: *mut Continuation,
    op: u64,
    a: *const u8,
    n: usize,
    _: *mut u8,
    r: usize,
) -> u8 {
    start_request(h, op, a, n, r, false, true)
}
unsafe extern "C" fn pipeline_output_start(
    h: *mut Continuation,
    op: u64,
    a: *const u8,
    n: usize,
    _: *mut u8,
    r: usize,
) -> u8 {
    start_request(h, op, a, n, r, true, true)
}
unsafe fn start_request(
    h: *mut Continuation,
    op: u64,
    a: *const u8,
    n: usize,
    r: usize,
    capture: bool,
    pipeline: bool,
) -> u8 {
    if a.is_null()
        || n != (if capture { 3 } else { 1 }) * WORD
        || r != (if pipeline {
            if capture {
                6
            } else {
                4
            }
        } else if capture {
            8
        } else {
            6
        }) * WORD
    {
        return 0;
    }
    let offsets = if capture { &[WORD][..] } else { &[][..] };
    if let Some(accepted) = super::provider::dispatch_handler(h, op, a, n, r, offsets) {
        return accepted;
    }
    let Some(continuation) = Continuation::retain_registered(h) else {
        return 0;
    };
    let decoded = if pipeline {
        protocol::decode_pipeline(a, capture)
    } else {
        protocol::decode(a, capture).map(|(command, input)| (vec![command], input))
    };
    let scope = current_or_default();
    let key = (h as usize, op);
    let cancelled = Arc::new(AtomicBool::new(false));
    {
        let mut pending = requests().lock().expect("process requests");
        if pending.contains_key(&key) {
            return 0;
        }
        pending.insert(key, cancelled.clone());
        // Serializes cancel-before-registration with the persistent marker.
        if continuation.is_cancelled() {
            cancelled.store(true, Ordering::Release);
        }
    }
    let registration = Registration(key);
    let cleanup = scope.begin_cleanup();
    let worker_continuation = continuation.clone();
    let task = std::thread::Builder::new()
        .name("joky-process".into())
        .spawn(move || {
            let _guard = scope.enter();
            let result = decoded.and_then(|(spec, input)| {
                if cancelled.load(Ordering::Acquire) {
                    return Err("process cancelled".into());
                }
                let commands = spec
                    .iter()
                    .enumerate()
                    .map(|(index, command)| {
                        runner::configure(command, &scope.env_snapshot.vars)
                            .map_err(|error| format!("process stage {}: {error}", index + 1))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                runner::run_pipeline(commands, input, &cancelled)
            });
            // Remove the old request before completion can reuse the continuation
            // for the same operation; a late cleanup must not erase the new entry.
            drop(registration);
            complete_request(
                &worker_continuation,
                key.0 as *mut Continuation,
                op,
                capture,
                pipeline,
                result,
            );
            drop(cleanup);
        });
    if let Err(error) = task {
        complete_request(
            &continuation,
            h,
            op,
            capture,
            pipeline,
            Err(format!("could not start process supervisor: {error}")),
        );
    }
    1
}

fn complete_request(
    c: &Continuation,
    h: *mut Continuation,
    op: u64,
    capture: bool,
    pipeline: bool,
    outcome: Result<runner::PipelineOutcome, String>,
) {
    if pipeline {
        protocol::complete_pipeline(c, h, op, capture, outcome);
    } else {
        protocol::complete(
            c,
            h,
            op,
            capture,
            outcome.map(|mut output| runner::Outcome {
                status: output.statuses.pop().expect("one command"),
                stdout: output.stdout,
                stderr: output.stderrs.pop().unwrap_or_default(),
            }),
        );
    }
}

#[cfg(test)]
mod tests;
