//! Independent local task captures. Preparation never creates executable slots;
//! the selected task acquires its execution storage on its first poll.
use super::{CpsEntry, CpsFrame, INVALID_STATE};
use crate::runtime::task_scope::layout::{CheckedLayout, VERSION};
use crate::{Error, ErrorCode, ErrorPhase};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    rc::{Rc, Weak},
};

thread_local! {
    static ENTRIES: RefCell<BTreeMap<u64, Weak<CheckedEntry>>> = const { RefCell::new(BTreeMap::new()) };
}

pub(super) struct CheckedEntry {
    _charge: Option<crate::runtime::parallel::Lease>,
    id: u64,
    pub(super) entry: CpsEntry,
    pub(super) frame_slots: usize,
    pub(super) layout: CheckedLayout,
    pub(super) inputs: Vec<(usize, usize)>,
    pub(super) result_key: Option<usize>,
    pub(super) result_lanes: usize,
}

impl Drop for CheckedEntry {
    fn drop(&mut self) {
        let _ = ENTRIES.try_with(|entries| entries.borrow_mut().remove(&self.id));
    }
}

/// Bind immutable module metadata to a non-reused entry token in this frame's
/// code snapshot. The native caller retains module and callback code through
/// cleanup, as required by the Local CPS ABI; this is not a parallel credential.
/// # Safety
/// Parent and descriptors are live on the serialized task thread. Descriptor
/// data is immutable; an indirect descriptor has the compiler-checked signature.
#[export_name = "__joky_task_entry_v1"]
pub(crate) unsafe extern "C" fn register(
    parent: *mut CpsFrame,
    descriptor: *const NativeTaskEntryV1,
    indirect: *const usize,
) -> u64 {
    let checked = || {
        let descriptor_ref = descriptor.as_ref().ok_or_else(|| {
            Error::new_with_code(
                ErrorPhase::Abi,
                ErrorCode::AbiError,
                "Missing task entry descriptor",
            )
        })?;
        let (entry, frame_slots, layout) = descriptor_ref.check(indirect)?;
        // Addresses only deduplicate immutable metadata within this live code
        // snapshot. They are never exposed or accepted as task/entry identities.
        let key = (descriptor as usize, entry as usize, frame_slots);
        if let Some(code) = (*parent).task_entries.get(&key) {
            return Ok(code.id);
        }
        if descriptor_ref.input_count > layout.slot_counts().0
            || (descriptor_ref.input_count != 0 && descriptor_ref.input_keys.is_null())
        {
            return Err(Error::new_with_code(
                ErrorPhase::Abi,
                ErrorCode::AbiError,
                "Invalid task capture ownership map",
            ));
        }
        let mut inputs = Vec::new();
        for index in 0..descriptor_ref.input_count {
            let from = *descriptor_ref.input_keys.add(index * 2);
            let to = *descriptor_ref.input_keys.add(index * 2 + 1);
            if to >= layout.slot_counts().0 || inputs.iter().any(|&(f, t)| f == from || t == to) {
                return Err(Error::new_with_code(
                    ErrorPhase::Abi,
                    ErrorCode::AbiError,
                    "Invalid task capture ownership map",
                ));
            }
            inputs.push((from, to));
        }
        let result_lanes = layout.result_lanes();
        let id = crate::runtime::task_scope::fresh_identity()?;
        let charge = crate::runtime::parallel::reserve_current(
            std::mem::size_of::<CheckedEntry>()
                + inputs.capacity() * std::mem::size_of::<(usize, usize)>()
                + 128,
        )?;
        let code = Rc::new(CheckedEntry {
            _charge: charge,
            id,
            entry,
            frame_slots,
            layout,
            inputs,
            result_key: (descriptor_ref.result_key != usize::MAX)
                .then_some(descriptor_ref.result_key),
            result_lanes,
        });
        ENTRIES.with(|entries| entries.borrow_mut().insert(id, Rc::downgrade(&code)));
        (*parent).task_entries.insert(key, code);
        Ok(id)
    };
    match checked() {
        Ok(id) => id,
        Err(error) => {
            crate::runtime::error::record_error(error);
            0
        }
    }
}

pub(super) fn lookup(parent: &CpsFrame, id: u64) -> Option<Rc<CheckedEntry>> {
    let code = ENTRIES.with(|entries| entries.borrow().get(&id).and_then(Weak::upgrade))?;
    parent
        .task_entries
        .values()
        .any(|entry| Rc::ptr_eq(entry, &code))
        .then_some(code)
}

/// Read-only module data, emitted with native relocations rather than compiler addresses.
#[repr(C)]
pub(crate) struct NativeTaskEntryV1 {
    pub version: usize,
    pub qualification: usize,
    pub entry: Option<CpsEntry>,
    pub frame_slots: usize,
    pub layout: *const u64,
    pub layout_words: usize,
    pub input_keys: *const usize,
    pub input_count: usize,
    pub result_key: usize,
}

impl NativeTaskEntryV1 {
    unsafe fn check(
        &self,
        indirect: *const usize,
    ) -> Result<(CpsEntry, usize, CheckedLayout), Error> {
        let bad = || {
            Error::new_with_code(
                ErrorPhase::Abi,
                ErrorCode::AbiError,
                "Invalid local task entry descriptor v1",
            )
        };
        if self.version != VERSION
            || self.qualification != 0
            || self.layout.is_null()
            || self.layout_words > 1_048_576
        {
            return Err(bad());
        }
        let layout =
            CheckedLayout::decode(std::slice::from_raw_parts(self.layout, self.layout_words))?;
        let (entry, frame_slots) = match (self.entry, indirect.is_null()) {
            (Some(entry), true) => (entry, self.frame_slots),
            (None, false) if *indirect != 0 => (
                std::mem::transmute::<usize, CpsEntry>(*indirect),
                *indirect.add(1),
            ),
            _ => return Err(bad()),
        };
        if frame_slots < layout.slot_counts().0
            || frame_slots > isize::MAX as usize / std::mem::size_of::<u64>()
        {
            return Err(bad());
        }
        Ok((entry, frame_slots, layout))
    }
}

pub(super) struct Prepared {
    frame_slots: usize,
    inputs: Box<CpsFrame>,
}

fn invalid(message: &str) {
    crate::runtime::error::record_error(Error::new_with_code(
        ErrorPhase::Runtime,
        ErrorCode::InvalidState,
        message,
    ));
}

pub(super) fn prepared(parent: &CpsFrame, code: Rc<CheckedEntry>) -> Result<Box<CpsFrame>, Error> {
    let input_slots = code.layout.slot_counts().0;
    let mut inputs = CpsFrame::try_new(input_slots)?;
    inputs.arena = parent.arena.clone();
    let mut child = CpsFrame::try_new(0)?;
    child.arena = parent.arena.clone();
    child.task_entry = Some(code.entry);
    child.prepared = Some(Prepared {
        frame_slots: code.frame_slots,
        inputs: Box::new(inputs),
    });
    child.task_code = Some(code);
    Ok(Box::new(child))
}

/// Prepare a dynamic task's capture packet without executing its entry.
/// # Safety
/// The parent is live and exclusive. The token belongs to its checked entry
/// snapshot. Entry and cleanup code remain alive through task teardown.
/// The returned task must be published exactly once with task_branch_commit.
#[export_name = "__joky_cps_task_prepare_v1"]
pub(crate) unsafe extern "C" fn prepare(parent: *mut CpsFrame, token: u64) -> *mut CpsFrame {
    let parent = &*parent;
    let live: usize = parent
        .task_scopes
        .iter()
        .map(|scope| scope.live_len())
        .sum();
    if live >= super::MAX_DYNAMIC_TASK_CHILDREN {
        crate::runtime::error::record_error(Error::new_with_code(
            ErrorPhase::Runtime,
            ErrorCode::ResourceLimit,
            "Dynamic task quota exceeded",
        ));
        return std::ptr::null_mut();
    }
    match lookup(parent, token) {
        Some(code) => match prepared(parent, code) {
            Ok(child) => Box::into_raw(child),
            Err(error) => {
                super::super::error::record_error(error);
                std::ptr::null_mut()
            }
        },
        None => {
            invalid("Unknown or foreign task entry token");
            std::ptr::null_mut()
        }
    }
}

/// Prepare one static task, owned by the parent even if later preparation fails.
/// # Safety
/// The parent is exclusive and has no published static group. Indices are dense
/// source positions. The entry token belongs to this parent's code snapshot,
/// whose entry and cleanup code remain alive until drain.
#[export_name = "__joky_cps_concurrent_prepare_v1"]
pub(crate) unsafe extern "C" fn prepare_static(
    parent: *mut CpsFrame,
    index: usize,
    token: u64,
) -> *mut CpsFrame {
    let parent = &mut *parent;
    if index == 0 && parent.children.iter().all(Option::is_none) {
        parent.children.clear();
    }
    if parent.concurrent.is_some() || index != parent.children.len() {
        invalid("Static task preparation must precede publication in source order");
        return std::ptr::null_mut();
    }
    let Some(code) = lookup(parent, token) else {
        invalid("Unknown or foreign task entry token");
        return std::ptr::null_mut();
    };
    let child = match prepared(parent, code) {
        Ok(child) => child,
        Err(error) => {
            super::super::error::record_error(error);
            return std::ptr::null_mut();
        }
    };
    parent.children.push(Some(child));
    let child = parent.children[index].as_deref_mut().unwrap();
    inputs(child)
}

/// Access a capture packet only while preparing its task, before publication.
/// # Safety
/// `child` comes from prepare, is exclusive and has not begun execution. The
/// returned pointer expires when the task starts or is destroyed.
#[export_name = "__joky_cps_task_inputs_v1"]
pub(crate) unsafe extern "C" fn inputs(child: *mut CpsFrame) -> *mut CpsFrame {
    match &mut (*child).prepared {
        Some(prepared) => &mut *prepared.inputs,
        None => {
            invalid("Task has no prepared input packet");
            std::ptr::null_mut()
        }
    }
}

/// Acquire execution storage and poll the entry frozen during preparation.
/// # Safety
/// `child` is a live published local task. The caller exclusively owns its poll
/// turn and keeps its code and borrowed resources alive through this call.
#[export_name = "__joky_cps_task_poll_v1"]
pub(crate) unsafe extern "C" fn poll(child: *mut CpsFrame) -> i32 {
    if (*child).task_polling || matches!((*child).state, super::DONE | super::FAILED) {
        invalid("Task cannot be polled while running or after termination");
        return INVALID_STATE;
    }
    let Some(entry) = (*child).task_entry else {
        invalid("Task has no registered entry");
        return INVALID_STATE;
    };
    if let Some(prepared) = &(*child).prepared {
        if let Err(error) = (*child).reserve_slots(prepared.frame_slots) {
            super::super::error::record_error(error);
            return super::RUNTIME_ERROR;
        }
    }
    if let Some(mut prepared) = (*child).prepared.take() {
        let mut slots = vec![0; prepared.frame_slots];
        slots[..prepared.inputs.slots.len()].copy_from_slice(&prepared.inputs.slots);
        (*child).slots = slots;
        (*child).cleanup = std::mem::replace(&mut prepared.inputs.cleanup, std::ptr::null_mut());
    }
    // No reference to metadata or capture storage spans generated/host code.
    (*child).task_polling = true;
    let status = entry(child);
    (*child).task_polling = false;
    status
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn task_scope_failure_drains_once_and_rejects_cleanup_reentry() {
        use std::cell::RefCell;
        thread_local! {
            static ROOT: Cell<*mut CpsFrame> = const { Cell::new(std::ptr::null_mut()) };
            static OBSERVED: RefCell<Vec<bool>> = const { RefCell::new(Vec::new()) };
        }
        unsafe extern "C" fn stop(_: *mut CpsFrame, _: u32) {
            let root = ROOT.with(Cell::get);
            let join = crate::runtime::error::checked(|| super::super::task_join(root)).is_err();
            let open =
                crate::runtime::error::checked(|| super::super::task_scope_enter(root)).is_err();
            let advance =
                crate::runtime::error::checked(|| super::super::task_round(root)).is_err();
            OBSERVED.with(|observed| observed.borrow_mut().extend([join, open, advance]));
            crate::runtime::error::record_error(Error::new_with_code(
                ErrorPhase::Runtime,
                ErrorCode::InvalidState,
                "Secondary stop failure",
            ));
        }
        unsafe extern "C" fn pending(frame: *mut CpsFrame) -> i32 {
            (*frame).host_stop = Some(stop);
            1
        }
        unsafe extern "C" fn failure(_: *mut CpsFrame) -> i32 {
            crate::runtime::error::record_error(Error::new_with_code(
                ErrorPhase::Runtime,
                ErrorCode::ResourceLimit,
                "Primary child failure",
            ));
            super::super::RUNTIME_ERROR
        }
        let ast = crate::parser::parse(
            r#"
            extern fn pending() @suspends
            extern fn failure() @suspends
            fn root() @suspends {
                scope {
                    branch { pending() }
                    branch { failure() }
                }
            }
        "#,
        )
        .unwrap();
        let module = crate::backends::jit::compile_with_registry(
            &ast,
            &[
                ("pending", pending as *const u8),
                ("failure", failure as *const u8),
            ],
            &Default::default(),
            vec![],
            crate::runtime::suspension::ModuleId(1),
            &Default::default(),
            &Default::default(),
        )
        .unwrap();
        let function = &module.exports["root"];
        let poll: CpsEntry = unsafe { std::mem::transmute(function.entry) };
        let mut frame = CpsFrame::new(function.info.frame_slots);
        ROOT.with(|root| root.set(&mut frame));
        let error = crate::runtime::error::checked(|| unsafe { poll(&mut frame) }).unwrap_err();
        ROOT.with(|root| root.set(std::ptr::null_mut()));
        assert_eq!(error.message(), "Primary child failure");
        assert!(frame.task_scopes.is_empty());
        crate::runtime::error::checked(|| frame.release()).unwrap();
        OBSERVED
            .with(|observed| assert_eq!(std::mem::take(&mut *observed.borrow_mut()), [true; 3]));
    }

    #[test]
    fn task_entry_tokens_reject_foreign_stale_and_invalid_layouts() {
        unsafe extern "C" fn entry(_: *mut CpsFrame) -> i32 {
            0
        }
        let words = crate::runtime::task_scope::layout::encode(&crate::FunctionSignature::new(
            vec![crate::Type::Int32],
            crate::Type::Int32,
        ))
        .unwrap();
        let descriptor = NativeTaskEntryV1 {
            version: VERSION,
            qualification: 0,
            entry: Some(entry),
            frame_slots: 1,
            layout: words.as_ptr(),
            layout_words: words.len(),
            input_keys: std::ptr::null(),
            input_count: 0,
            result_key: usize::MAX,
        };
        let mut first = CpsFrame::new(0);
        let mut second = CpsFrame::new(0);
        unsafe {
            let token = register(&mut first, &descriptor, std::ptr::null());
            assert_ne!(token, 0);
            assert_eq!(register(&mut first, &descriptor, std::ptr::null()), token);
            assert!(crate::runtime::error::checked(|| prepare(&mut second, token)).is_err());
            drop(first);
            assert!(ENTRIES.with(|entries| !entries.borrow().contains_key(&token)));
            assert!(crate::runtime::error::checked(|| prepare(&mut second, token)).is_err());
            let next = register(&mut second, &descriptor, std::ptr::null());
            assert!(next > token);
            for (version, qualification, slots, length) in [
                (VERSION + 1, 0, 1, words.len()),
                (VERSION, 1, 1, words.len()),
                (VERSION, 0, 0, words.len()),
                (VERSION, 0, usize::MAX, words.len()),
                (VERSION, 0, 1, words.len() - 1),
            ] {
                let invalid = NativeTaskEntryV1 {
                    version,
                    qualification,
                    entry: Some(entry),
                    frame_slots: slots,
                    layout: words.as_ptr(),
                    layout_words: length,
                    input_keys: std::ptr::null(),
                    input_count: 0,
                    result_key: usize::MAX,
                };
                assert!(crate::runtime::error::checked(|| register(
                    &mut second,
                    &invalid,
                    std::ptr::null()
                ))
                .is_err());
            }
            assert_eq!(second.task_entries.len(), 1);
        }
    }

    #[test]
    fn task_capture_packets_freeze_inputs_and_indirect_entries_before_execution() {
        thread_local! {
            static ROOT: Cell<*mut CpsFrame> = const { Cell::new(std::ptr::null_mut()) };
            static ORIGINAL: Cell<u64> = const { Cell::new(0) };
            static REPLACEMENT: Cell<u64> = const { Cell::new(0) };
            static PREPARED: Cell<bool> = const { Cell::new(false) };
        }
        unsafe extern "C" fn probe(frame: *mut CpsFrame) -> i32 {
            let root = ROOT.with(Cell::get);
            let pending = (&(*root).children)[1].as_ref().unwrap();
            PREPARED.with(|observed| {
                observed.set(
                    pending.slots.is_empty()
                        && pending.prepared.as_ref().is_some_and(|prepared| {
                            prepared.inputs.slots == [41] && prepared.frame_slots > 0
                        }),
                );
            });
            let old = ORIGINAL.with(Cell::get);
            let new = REPLACEMENT.with(Cell::get);
            for slot in &mut (*root).slots {
                if *slot == old {
                    *slot = new;
                } else if *slot == 41 {
                    *slot = 9000;
                }
            }
            (*frame).result = 0;
            0
        }
        let ast = crate::parser::parse(
            r#"
            extern fn probe(value: Int32) @suspends -> Int32
            fn increment(value: Int32) @suspends -> Int32 { value + 1 }
            fn replacement(value: Int32) @suspends -> Int32 { value + 100 }
            fn root(f: fn(Int32) @suspends -> Int32, value: Int32) @suspends -> Int32 {
                let (a, b) = join { | probe(value) | f(value) }
                a + b
            }
            fn invoke(value: Int32) @suspends -> Int32 {
                let f = root
                f(increment, value)
            }
            "#,
        )
        .unwrap();
        let module = crate::backends::jit::compile_with_registry(
            &ast,
            &[("probe", probe as *const u8)],
            &Default::default(),
            vec![],
            crate::runtime::suspension::ModuleId(1),
            &Default::default(),
            &Default::default(),
        )
        .unwrap();
        let original = &module.exports["increment"];
        let replacement = &module.exports["replacement"];
        let original = [original.entry as usize, original.info.frame_slots];
        let replacement = [replacement.entry as usize, replacement.info.frame_slots];
        let function = &module.exports["root"];
        let entry: CpsEntry = unsafe { std::mem::transmute(function.entry) };
        let mut frame = CpsFrame::new(function.info.frame_slots);
        frame.slots[0] = original.as_ptr() as usize as u64;
        frame.slots[1] = 41;
        ROOT.with(|root| root.set(&mut frame));
        ORIGINAL.with(|slot| slot.set(original.as_ptr() as usize as u64));
        REPLACEMENT.with(|slot| slot.set(replacement.as_ptr() as usize as u64));
        let status = crate::runtime::error::checked(|| unsafe { entry(&mut frame) }).unwrap();
        ROOT.with(|root| root.set(std::ptr::null_mut()));
        assert_eq!(status, 0);
        assert!(PREPARED.with(Cell::get));
        assert_eq!(frame.result, 42);
    }
}
