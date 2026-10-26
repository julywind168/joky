//! Versioned Local task protocol. Tokens identify registrations, never addresses.
//! The checked compiler supplies the local polling driver; this module owns
//! preparation, publication, terminal barriers and one-time result delivery.
use super::{task, CpsFrame};
use crate::{Error, ErrorCode, ErrorPhase};
use std::{cell::Cell, rc::Rc};

thread_local! { static CONTEXT: Cell<*mut CpsFrame> = const { Cell::new(std::ptr::null_mut()) }; }

pub(super) struct Scope {
    pub(super) remote: Option<crate::runtime::parallel::pool::Group>,
    _charge: Option<crate::runtime::parallel::Lease>,
    id: u64,
    kind: u32,
    capacity: usize,
    prepared: Vec<(u64, Box<CpsFrame>)>,
    entries: Vec<Rc<task::CheckedEntry>>,
    published: bool,
    closed: bool,
    terminal: Option<i32>,
    failure: Option<Error>,
    draining: bool,
    drained: bool,
}

impl Scope {
    pub(super) fn take_prepared(&mut self) -> Vec<(u64, Box<CpsFrame>)> {
        std::mem::take(&mut self.prepared)
    }
}

pub(super) fn remote(frame: &mut CpsFrame) -> Option<&mut crate::runtime::parallel::pool::Group> {
    frame
        .task_protocol
        .iter_mut()
        .rev()
        .find(|scope| scope.kind != 2)?
        .remote
        .as_mut()
}
pub(super) fn cancelled(frame: &CpsFrame) -> bool {
    frame
        .task_protocol
        .last()
        .is_some_and(|scope| scope.terminal == Some(3))
}

fn invalid(message: &str) -> Error {
    Error::new_with_code(ErrorPhase::Runtime, ErrorCode::InvalidState, message)
}
fn report(error: Error) -> i32 {
    crate::runtime::error::record_error(error);
    2
}
unsafe fn parent() -> Result<*mut CpsFrame, Error> {
    let frame = CONTEXT.with(Cell::get);
    if frame.is_null() {
        Err(invalid("Missing task execution context"))
    } else {
        Ok(frame)
    }
}
unsafe fn lookup(token: u64) -> Result<(*mut CpsFrame, usize), Error> {
    let frame = parent()?;
    let index = (*frame)
        .task_protocol
        .iter()
        .position(|scope| scope.id == token)
        .ok_or_else(|| invalid("Unknown, foreign or released task scope token"))?;
    if index + 1 != (*frame).task_protocol.len() {
        return Err(invalid("An inner task scope is still active"));
    }
    Ok((frame, index))
}

/// # Safety
/// Compiler-only context binding; the frame stays live through every following
/// protocol call. Generated code rebinds after any potentially nested callback.
#[export_name = "__joky_task_context_v1"]
pub(crate) unsafe extern "C" fn context(frame: *mut CpsFrame) {
    CONTEXT.with(|slot| slot.set(frame));
}

/// # Safety
/// The checked lowering owns the current Local frame and uses a valid scope kind.
#[export_name = "__joky_task_scope_open_v1"]
pub(crate) unsafe extern "C" fn open(kind: u32, capacity: usize) -> u64 {
    let result = (|| {
        let frame = parent()?;
        if (*frame).state == super::FAILED
            || kind > 2
            || capacity > super::MAX_DYNAMIC_TASK_CHILDREN
            || (kind != 2 && capacity == 0)
            || (*frame).task_protocol.iter().any(|scope| scope.draining)
            || (kind != 2 && (*frame).task_protocol.iter().any(|scope| scope.kind != 2))
        {
            return Err(invalid("Invalid task scope admission"));
        }
        let id = crate::runtime::task_scope::fresh_identity()?;
        let charge = crate::runtime::parallel::reserve_current(
            std::mem::size_of::<Scope>()
                + capacity
                    * (std::mem::size_of::<(u64, Box<CpsFrame>)>()
                        + std::mem::size_of::<Rc<task::CheckedEntry>>()),
        )?;
        let mut prepared = Vec::new();
        prepared
            .try_reserve(capacity)
            .map_err(|_| invalid("Task scope capacity exhausted"))?;
        if kind == 2 {
            super::task_scope_enter(frame);
        }
        (*frame).task_protocol.push(Scope {
            remote: crate::runtime::parallel::pool::active()
                .then(|| crate::runtime::parallel::pool::Group::new(kind)),
            _charge: charge,
            id,
            kind,
            capacity,
            prepared,
            entries: Vec::new(),
            published: false,
            closed: false,
            terminal: None,
            failure: None,
            draining: false,
            drained: false,
        });
        Ok(id)
    })();
    match result {
        Ok(id) => id,
        Err(error) => {
            report(error);
            0
        }
    }
}

/// # Safety
/// Called only by checked lowering, with a live context bound immediately before.
#[export_name = "__joky_task_current_scope_v1"]
pub(crate) unsafe extern "C" fn current(kind: u32) -> u64 {
    match parent().and_then(|frame| {
        (*frame)
            .task_protocol
            .iter()
            .rev()
            .find(|scope| (scope.kind == 2) == (kind == 2))
            .map(|scope| scope.id)
            .ok_or_else(|| invalid("Missing current task scope"))
    }) {
        Ok(id) => id,
        Err(error) => {
            report(error);
            0
        }
    }
}

/// # Safety
/// Lanes describe the registered entry's complete input layout. The caller owns
/// every input cleanup before this call and must not use moved owners on success.
#[export_name = "__joky_task_child_prepare_v1"]
pub(crate) unsafe extern "C" fn prepare(
    scope: u64,
    entry: u64,
    lanes: *const u64,
    count: usize,
) -> u64 {
    let result = (|| {
        let (frame, index) = lookup(scope)?;
        let record = &(&(*frame).task_protocol)[index];
        if (*frame).state == super::FAILED
            || record.closed
            || record.terminal.is_some()
            || record.draining
            || (record.kind != 2 && record.published)
            || (record.kind != 2 && record.prepared.len() >= record.capacity)
        {
            return Err(invalid("Task scope is closed to preparation"));
        }
        if record.kind == 2
            && (*frame)
                .task_scopes
                .iter()
                .map(|scope| scope.live_len())
                .sum::<usize>()
                + record.prepared.len()
                >= super::MAX_DYNAMIC_TASK_CHILDREN
        {
            return Err(invalid("Dynamic task quota exceeded"));
        }
        let code = task::lookup(&*frame, entry)
            .ok_or_else(|| invalid("Unknown or foreign task entry token"))?;
        if code.layout.slot_counts().0 != count || (count != 0 && lanes.is_null()) {
            return Err(invalid("Task input packet layout mismatch"));
        }
        let id = crate::runtime::task_scope::fresh_identity()?;
        (&mut (*frame).task_protocol)[index]
            .prepared
            .try_reserve(1)
            .map_err(|_| invalid("Task preparation capacity exhausted"))?;
        // All fallible snapshots precede the first ownership transfer.
        for &(from, _) in &code.inputs {
            (*super::cleanup(frame)).entries.snapshot(from)?;
        }
        let mut child = task::prepared(&*frame, code.clone())?;
        let inputs = task::inputs(&mut *child);
        if count != 0 {
            (*inputs)
                .slots
                .copy_from_slice(std::slice::from_raw_parts(lanes, count));
        }
        for (base, layout) in code.layout.input_places() {
            let source = (&(*inputs).slots)[base] as *const u8;
            let target = super::cleanup(inputs);
            let destination = (*target).entries.places.materialize(
                &mut *(*target).arena,
                base,
                layout.size,
                layout.align,
            )?;
            crate::runtime::place::copy(destination.as_ptr(), source, layout.nodes);
            (&mut (*inputs).slots)[base] = destination.as_ptr() as u64;
        }
        for &(from, to) in &code.inputs {
            super::transfer_owner(frame, inputs, from, to);
        }
        (&mut (*frame).task_protocol)[index]
            .prepared
            .push((id, child));
        Ok(id)
    })();
    match result {
        Ok(id) => id,
        Err(error) => {
            report(error);
            0
        }
    }
}

/// # Safety
/// Tokens are this scope's prepared children, in source order. No callback runs
/// between validation and the complete batch publication.
#[export_name = "__joky_task_publish_v1"]
pub(crate) unsafe extern "C" fn publish(scope: u64, children: *const u64, count: usize) -> i32 {
    let result = (|| {
        let (frame, index) = lookup(scope)?;
        let record = &(&(*frame).task_protocol)[index];
        if (*frame).state == super::FAILED
            || children.is_null()
            || count == 0
            || record.closed
            || record.terminal.is_some()
            || record.draining
            || record.prepared.len() != count
            || (record.kind == 2 && count != 1)
            || (record.kind != 2 && (record.published || count != record.capacity))
        {
            return Err(invalid("Invalid task publication batch"));
        }
        let tokens = std::slice::from_raw_parts(children, count);
        if !record
            .prepared
            .iter()
            .zip(tokens)
            .all(|((id, _), token)| id == token)
        {
            return Err(invalid(
                "Publication contains foreign, repeated or reordered child tokens",
            ));
        }
        if record.kind != 2
            && record.remote.is_some()
            && record.prepared.iter().any(|(_, child)| {
                child
                    .task_code
                    .as_ref()
                    .unwrap()
                    .layout
                    .parallel_shapes()
                    .is_err()
            })
        {
            (&mut (*frame).task_protocol)[index].remote = None;
        }
        let record = &(&(*frame).task_protocol)[index];
        if record.remote.is_some() {
            let prepared = std::mem::take(&mut (&mut (*frame).task_protocol)[index].prepared);
            let mut factories = Vec::new();
            let mut entries = Vec::new();
            for (_, child) in prepared {
                let code = child.task_code.as_ref().unwrap().clone();
                factories.push(super::parallel::factory(child, &code)?);
                entries.push(code);
            }
            let record = &mut (&mut (*frame).task_protocol)[index];
            record.remote.as_mut().unwrap().publish(factories)?;
            record.entries.extend(entries);
            record.published = true;
            if record.kind != 2 {
                (*frame).concurrent_results = (0..count).map(|_| None).collect();
            }
            return Ok(());
        }
        if record.kind == 2 {
            let child = (&mut (*frame).task_protocol)[index]
                .prepared
                .pop()
                .unwrap()
                .1;
            let dynamic = (*frame)
                .task_scopes
                .last_mut()
                .ok_or_else(|| invalid("Missing dynamic scope"))?;
            if let Err((error, child)) = dynamic.publish(child) {
                (&mut (*frame).task_protocol)[index]
                    .prepared
                    .push((tokens[0], child));
                return Err(error);
            }
        } else {
            let group = crate::runtime::task_scope::LocalTaskGroup::new(
                if record.kind == 0 {
                    crate::runtime::task_scope::CompletionPolicy::All
                } else {
                    crate::runtime::task_scope::CompletionPolicy::First
                },
                count,
            )?;
            let children = std::mem::take(&mut (&mut (*frame).task_protocol)[index].prepared);
            let entries: Vec<_> = children
                .iter()
                .map(|(_, child)| child.task_code.as_ref().unwrap().clone())
                .collect();
            (*frame).children = children.into_iter().map(|(_, child)| Some(child)).collect();
            (*frame).concurrent = Some(group);
            (*frame).concurrent_results = (0..count).map(|_| None).collect();
            (&mut (*frame).task_protocol)[index].entries = entries;
        }
        (&mut (*frame).task_protocol)[index].published = true;
        Ok(())
    })();
    match result {
        Ok(()) => 0,
        Err(error) => report(error),
    }
}

/// # Safety
/// The Local driver has returned its polling turn before joining. Static driver
/// selection is shared with the interpreter; Pending never authorizes delivery.
#[export_name = "__joky_task_join_v1"]
pub(crate) unsafe extern "C" fn join(scope: u64) -> i32 {
    let (frame, index) = match lookup(scope) {
        Ok(value) => value,
        Err(error) => return report(error),
    };
    let record = &(&(*frame).task_protocol)[index];
    if record.draining {
        return report(invalid("Cannot reenter task cleanup"));
    }
    if let Some(status) = record.terminal {
        return record.failure.clone().map_or(status, report);
    }
    if !record.prepared.is_empty() {
        return report(invalid("Unpublished task captures remain"));
    }
    if record.remote.is_some() {
        let record = &mut (&mut (*frame).task_protocol)[index];
        record.closed = true;
        return match record.remote.as_mut().unwrap().poll() {
            Ok(false) => 1,
            Ok(true) => {
                record.terminal = Some(0);
                record.drained = true;
                0
            }
            Err(error) => {
                record.terminal = Some(2);
                record.drained = true;
                record.failure = Some(error.clone());
                report(error)
            }
        };
    }
    let kind = record.kind;
    if kind != 2 && !record.published {
        return report(invalid("Static tasks have not been published"));
    }
    (&mut (*frame).task_protocol)[index].closed = true;
    if kind != 2
        && !(*frame)
            .concurrent
            .as_ref()
            .is_some_and(|group| group.is_selected())
    {
        return 1;
    }
    (&mut (*frame).task_protocol)[index].draining = true;
    let result = crate::runtime::error::checked(|| {
        if kind == 2 {
            super::task_join(frame)
        } else {
            super::concurrent::drain(frame)
        }
    });
    context(frame);
    let record = &mut (&mut (*frame).task_protocol)[index];
    record.draining = false;
    match result {
        Ok(1) => 1,
        Ok(0) => {
            record.terminal = Some(0);
            record.drained = true;
            0
        }
        Ok(_) => {
            record.terminal = Some(2);
            record.drained = true;
            let error = invalid("Task join failed");
            record.failure = Some(error.clone());
            report(error)
        }
        Err(error) => {
            record.terminal = Some(2);
            record.drained = true;
            record.failure = Some(error.clone());
            report(error)
        }
    }
}

/// # Safety
/// The output is writable for exactly count lanes. It is untouched on failure.
#[export_name = "__joky_task_take_result_v1"]
pub(crate) unsafe extern "C" fn take(
    scope: u64,
    child_index: usize,
    lanes: *mut u64,
    count: usize,
) -> i32 {
    let result = (|| {
        let (frame, index) = lookup(scope)?;
        let record = &(&(*frame).task_protocol)[index];
        if let Some(error) = &record.failure {
            return Err(error.clone());
        }
        if (*frame).state == super::FAILED
            || record.kind == 2
            || record.terminal != Some(0)
            || !record.drained
            || record.draining
        {
            return Err(invalid("Task scope is not ready to deliver results"));
        }
        if let Some(remote) = &record.remote {
            let source = if record.kind == 1 {
                remote
                    .winner()
                    .ok_or_else(|| invalid("Missing race winner"))?
            } else {
                child_index
            };
            if record.kind == 1 && child_index != 0 {
                return Err(invalid("Race exposes only its winning result"));
            }
            let entry = record
                .entries
                .get(source)
                .ok_or_else(|| invalid("Unknown parallel result"))?;
            if entry.result_lanes != count || (count != 0 && lanes.is_null()) {
                return Err(invalid("Task output packet layout mismatch"));
            }
            let key = entry.result_key;
            let output = (&mut (*frame).task_protocol)[index]
                .remote
                .as_mut()
                .unwrap()
                .take(source)?;
            return super::parallel::take(output, frame, key, lanes, count);
        }
        let group = (*frame)
            .concurrent
            .as_ref()
            .ok_or_else(|| invalid("Missing task group"))?;
        let source = if record.kind == 1 {
            if child_index != 0 {
                return Err(invalid("Race exposes only its winning result"));
            }
            *group
                .completion_order()
                .first()
                .ok_or_else(|| invalid("Missing race winner"))?
        } else {
            child_index
        };
        let entry = record
            .entries
            .get(source)
            .ok_or_else(|| invalid("Unknown task result"))?
            .clone();
        if entry.result_lanes != count || (count != 0 && lanes.is_null()) {
            return Err(invalid("Task output packet layout mismatch"));
        }
        let packets = &mut (*frame).concurrent_results;
        let packet = packets
            .get_mut(source)
            .and_then(Option::as_deref_mut)
            .ok_or_else(|| invalid("Missing task result packet"))?
            as *mut CpsFrame;
        (*frame).concurrent.as_mut().unwrap().take_result(source)?;
        if let Some(layout) = entry.layout.result_place() {
            crate::runtime::place::copy(
                *lanes as *mut u8,
                (&(*packet).slots)[0] as *const u8,
                layout.nodes,
            );
        } else if count != 0 {
            std::ptr::copy_nonoverlapping((*packet).slots.as_ptr(), lanes, count);
        }
        if let Some(key) = entry.result_key {
            super::transfer_owner(packet, frame, key, key);
        }
        Ok(())
    })();
    match result {
        Ok(()) => 0,
        Err(error) => report(error),
    }
}

/// # Safety
/// Requests run only when the current Local task has returned its polling turn.
#[export_name = "__joky_task_request_cancel_v1"]
pub(crate) unsafe extern "C" fn cancel(scope: u64) -> i32 {
    match lookup(scope) {
        Ok((frame, index)) => {
            let record = &mut (&mut (*frame).task_protocol)[index];
            if record.draining {
                return report(invalid("Cannot cancel during task cleanup"));
            }
            if record.terminal.is_some() {
                return 0;
            }
            if record.kind != 2
                && (*frame)
                    .concurrent
                    .as_ref()
                    .is_some_and(|group| group.is_selected())
            {
                return 0;
            }
            if let Some(remote) = &record.remote {
                remote.cancel();
            }
            record.terminal.get_or_insert(3);
            record.closed = true;
            if record.kind == 2 {
                if let Some(dynamic) = (*frame).task_scopes.last_mut() {
                    dynamic.cancel();
                }
            }
            if let Some(waker) = crate::runtime::executor::current_waker() {
                waker.wake();
            }
            0
        }
        Err(error) => report(error),
    }
}

/// # Safety
/// Local stop is synchronous; no running poll or callback may overlap this call.
#[export_name = "__joky_task_drain_v1"]
pub(crate) unsafe extern "C" fn drain(scope: u64) -> i32 {
    let (frame, index) = match lookup(scope) {
        Ok(value) => value,
        Err(error) => return report(error),
    };
    let record = &(&(*frame).task_protocol)[index];
    if record.draining {
        return report(invalid("Cannot reenter task cleanup"));
    }
    if record.drained {
        return record
            .failure
            .clone()
            .map_or(record.terminal.unwrap_or(0), report);
    }
    if record.terminal.is_none() {
        return join(scope);
    }
    if record.remote.is_some() {
        let record = &mut (&mut (*frame).task_protocol)[index];
        return match record.remote.as_mut().unwrap().poll() {
            Ok(false) => 1,
            Ok(true) => {
                record.drained = true;
                record.terminal.unwrap_or(0)
            }
            Err(error) => {
                record.drained = true;
                report(error)
            }
        };
    }
    let kind = record.kind;
    if (*frame)
        .children
        .iter()
        .flatten()
        .any(|child| child.task_polling)
        || (*frame)
            .task_scopes
            .iter()
            .any(|scope| scope.entries().any(|(_, child, _)| child.task_polling))
    {
        return report(invalid("Cannot drain a running task"));
    }
    if kind != 2 {
        if let Some(group) = &mut (*frame).concurrent {
            group.stop(None);
        }
    } else if let Some(dynamic) = (*frame).task_scopes.last_mut() {
        dynamic.cancel();
    }
    (&mut (*frame).task_protocol)[index].draining = true;
    let prepared = std::mem::take(&mut (&mut (*frame).task_protocol)[index].prepared);
    let outcome = crate::runtime::error::checked(|| {
        drop(prepared);
        if kind == 2 {
            if let Some(last) = (*frame).task_scopes.len().checked_sub(1) {
                if let Err(error) = super::drain_dynamic_scope(frame, last) {
                    report(error);
                }
                (*frame).task_scopes.pop();
            }
        } else if (*frame).concurrent.is_some() {
            super::concurrent::drain(frame);
        }
    });
    context(frame);
    let record = &mut (&mut (*frame).task_protocol)[index];
    record.draining = false;
    record.drained = true;
    if let Err(error) = outcome {
        if record.terminal != Some(3) {
            record.terminal = Some(2);
            record.failure.get_or_insert(error);
        }
    }
    record
        .failure
        .clone()
        .map_or(record.terminal.unwrap_or(0), report)
}

/// # Safety
/// Release follows drain and all successful result transfers. Token use after
/// release is rejected even when subsequent allocations reuse storage addresses.
#[export_name = "__joky_task_scope_release_v1"]
pub(crate) unsafe extern "C" fn release(scope: u64) -> i32 {
    let (frame, index) = match lookup(scope) {
        Ok(value) => value,
        Err(error) => return report(error),
    };
    let record = &(&(*frame).task_protocol)[index];
    if record.draining || !record.drained || index + 1 != (*frame).task_protocol.len() {
        return report(invalid("Cannot release a live or enclosing task scope"));
    }
    let kind = record.kind;
    let remote = record.remote.is_some();
    let record = (*frame).task_protocol.pop().unwrap();
    if remote && kind == 2 {
        (*frame).task_scopes.pop();
    }
    let outcome = crate::runtime::error::checked(|| {
        if kind != 2 {
            super::release_children(frame);
        }
        drop(record);
    });
    match outcome {
        Ok(()) => 0,
        Err(error) => report(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_protocol_tokens_batch_publication_and_exactly_once_delivery() {
        unsafe extern "C" fn ready(frame: *mut CpsFrame) -> i32 {
            (*frame).result = (&(*frame).slots)[0] + 1;
            0
        }
        let words = crate::runtime::task_scope::layout::encode(&crate::FunctionSignature::new(
            vec![crate::Type::Int32],
            crate::Type::Int32,
        ))
        .unwrap();
        let descriptor = task::NativeTaskEntryV1 {
            version: 1,
            qualification: 0,
            entry: Some(ready),
            frame_slots: 1,
            layout: words.as_ptr(),
            layout_words: words.len(),
            input_keys: std::ptr::null(),
            input_count: 0,
            result_key: usize::MAX,
        };
        let mut frame = CpsFrame::new(1);
        let mut foreign = CpsFrame::new(0);
        unsafe {
            context(&mut frame);
            let entry = task::register(&mut frame, &descriptor, std::ptr::null());
            let scope = open(0, 2);
            let a = prepare(scope, entry, [40].as_ptr(), 1);
            let b = prepare(scope, entry, [41].as_ptr(), 1);
            assert_ne!(a, 0);
            assert_ne!(a, b);
            context(&mut foreign);
            assert!(crate::runtime::error::checked(|| cancel(scope)).is_err());
            context(&mut frame);
            assert!(crate::runtime::error::checked(|| publish(scope, [a, a].as_ptr(), 2)).is_err());
            assert!(frame.children.is_empty());
            assert_eq!(frame.task_protocol[0].prepared.len(), 2);
            assert_eq!(publish(scope, [a, b].as_ptr(), 2), 0);
            assert!(crate::runtime::error::checked(|| publish(scope, [a, b].as_ptr(), 2)).is_err());
            assert!(
                crate::runtime::error::checked(|| prepare(scope, entry, [99].as_ptr(), 1)).is_err()
            );
            let mut output = [999u64; 2];
            assert!(
                crate::runtime::error::checked(|| take(scope, 0, output.as_mut_ptr(), 1)).is_err()
            );
            assert_eq!(output, [999; 2]);
            assert_eq!(join(scope), 1);
            for index in 0..2 {
                assert_eq!(super::super::concurrent::next(&mut frame), index as i64);
                let child = frame.children[index].as_deref_mut().unwrap() as *mut CpsFrame;
                assert_eq!(task::poll(child), 0);
                let packet = super::super::concurrent::result(&mut frame, index, 2);
                (&mut (*packet).slots)[0] = (*child).result;
                assert_eq!(super::super::concurrent::polled(&mut frame, index, 0), 0);
            }
            assert_eq!(join(scope), 0);
            assert_eq!(drain(scope), 0);
            assert!(
                crate::runtime::error::checked(|| take(scope, 0, output.as_mut_ptr(), 2)).is_err()
            );
            assert_eq!(output, [999; 2]);
            assert_eq!(take(scope, 0, output.as_mut_ptr(), 1), 0);
            assert_eq!(output, [41, 999]);
            assert!(
                crate::runtime::error::checked(|| take(scope, 0, output.as_mut_ptr(), 1)).is_err()
            );
            assert_eq!(release(scope), 0);
            assert!(crate::runtime::error::checked(|| join(scope)).is_err());
            let next = open(0, 1);
            assert!(next > b);
            let prepared = prepare(next, entry, [7].as_ptr(), 1);
            assert_ne!(prepared, 0);
            assert_eq!(cancel(next), 0);
            assert_eq!(drain(next), 3);
            assert_eq!(drain(next), 3);
            assert!(
                crate::runtime::error::checked(|| take(next, 0, output.as_mut_ptr(), 1)).is_err()
            );
            assert_eq!(release(next), 0);
            let cancelled = open(1, 1);
            let child = prepare(cancelled, entry, [7].as_ptr(), 1);
            assert_eq!(publish(cancelled, [child].as_ptr(), 1), 0);
            assert_eq!(cancel(cancelled), 0);
            assert_eq!(
                super::super::concurrent::next(&mut frame),
                super::super::concurrent::SELECTED
            );
            assert!(frame.children[0].as_ref().unwrap().slots.is_empty());
            assert_eq!(drain(cancelled), 3);
            assert_eq!(release(cancelled), 0);
            unsafe extern "C" fn failure(_: *mut CpsFrame) -> i32 {
                report(invalid("Original task failure"))
            }
            let descriptor = task::NativeTaskEntryV1 {
                entry: Some(failure),
                ..descriptor
            };
            let failed_entry = task::register(&mut frame, &descriptor, std::ptr::null());
            let outer_scope = open(2, 0);
            let failed_scope = open(2, 0);
            let child = prepare(failed_scope, failed_entry, [7].as_ptr(), 1);
            assert_eq!(publish(failed_scope, [child].as_ptr(), 1), 0);
            for _ in 0..2 {
                let error = crate::runtime::error::checked(|| join(failed_scope)).unwrap_err();
                assert_eq!(error.message(), "Original task failure");
                let error = crate::runtime::error::checked(|| drain(failed_scope)).unwrap_err();
                assert_eq!(error.message(), "Original task failure");
            }
            let error =
                crate::runtime::error::checked(|| take(failed_scope, 0, output.as_mut_ptr(), 1))
                    .unwrap_err();
            assert_eq!(error.message(), "Original task failure");
            assert_eq!(output, [41, 999]);
            assert_eq!(cancel(failed_scope), 0);
            assert_eq!(release(failed_scope), 0);
            let outer_child = prepare(outer_scope, entry, [41].as_ptr(), 1);
            assert_ne!(outer_child, 0);
            assert_eq!(publish(outer_scope, [outer_child].as_ptr(), 1), 0);
            assert_eq!(join(outer_scope), 0);
            assert_eq!(release(outer_scope), 0);
            context(std::ptr::null_mut());
        }
    }
}
