//! P0 experiment, not a production promotion API. The caller supplies the full
//! allocation set; discovering nested owners and descriptors belongs to P2.
use super::*;
use crate::runtime::{
    buffer::Buffer,
    cleanup::{Cleanups, Entry},
    owned_text,
};

fn transfer(
    from: &mut Arena,
    to: &mut Arena,
    selected: &[NonNull<u8>],
    limit: usize,
    fail_at: Option<usize>,
) -> Result<(), Error> {
    let checkpoint = |step| {
        if fail_at == Some(step) {
            Err(allocation_error())
        } else {
            Ok(())
        }
    };
    checkpoint(0)?;
    let mut charge = 0usize;
    for (i, pointer) in selected.iter().enumerate() {
        if selected[..i].contains(pointer) || to.contains(*pointer) {
            return Err(allocation_error());
        }
        let (_, layout) = from
            .allocations
            .iter()
            .find(|(p, _)| p == pointer)
            .ok_or_else(allocation_error)?;
        charge = charge
            .checked_add(
                layout.pad_to_align().size() + std::mem::size_of::<(NonNull<u8>, Layout)>(),
            )
            .ok_or_else(allocation_error)?;
    }
    let bytes = to
        .bytes
        .checked_add(charge)
        .filter(|n| *n <= limit)
        .ok_or_else(allocation_error)?;
    checkpoint(1)?;
    to.allocations
        .try_reserve(selected.len())
        .map_err(|_| allocation_error())?;
    checkpoint(2)?;
    // Commit is allocation-free, non-suspending, and invokes no user code.
    for pointer in selected {
        let index = from
            .allocations
            .iter()
            .position(|(p, _)| p == pointer)
            .unwrap();
        to.allocations.push(from.allocations.swap_remove(index));
    }
    from.bytes -= charge;
    to.bytes = bytes;
    Ok(())
}

#[test]
fn persistent_probe_transfers_owning_storage_and_cleanup_without_copying_payloads() {
    for fault in [Some(0), Some(1), Some(2), None] {
        let mut task = Arena::default();
        let mut persistent = Arena::default();
        let temporary = task.allocate(17, 8).unwrap();
        let start = task.allocations.len();
        let text = Buffer::copy_bytes(&mut task, b"persistent").unwrap();
        let selected: Vec<_> = task.allocations[start..].iter().map(|(p, _)| *p).collect();
        let mut cleanups = Cleanups::default();
        cleanups.insert(
            0,
            Entry {
                keep_alive: None,
                storage_size: 0,
                storage: None,
                owners: vec![Some(text.as_ptr())],
                hooks: vec![],
            },
        );
        let original = task.bytes;
        assert!(transfer(&mut task, &mut persistent, &selected, 0, None).is_err());
        assert!(transfer(
            &mut task,
            &mut persistent,
            &[selected[0], selected[0]],
            MAX_BYTES,
            None
        )
        .is_err());
        let result = transfer(&mut task, &mut persistent, &selected, MAX_BYTES, fault);
        assert_eq!(task.bytes + persistent.bytes, original);
        assert!(task.contains(temporary));
        if fault.is_some() {
            assert!(result.is_err());
            assert_eq!(task.bytes, original);
            assert!(persistent.allocations.is_empty());
            unsafe {
                cleanups.finish(&mut task).unwrap();
            }
        } else {
            result.unwrap();
            let entry = cleanups.take(0).unwrap();
            unsafe {
                cleanups.finish(&mut task).unwrap();
            }
            assert_eq!(task.allocations.len(), 1);
            drop(task);
            assert_eq!(
                unsafe { owned_text::borrow(text.as_ptr()).unwrap() },
                "persistent"
            );
            for pointer in &selected {
                assert!(persistent.contains(*pointer));
            }
            unsafe {
                entry.run(&mut persistent).unwrap();
            }
            assert_eq!(persistent.bytes, 0);
            assert!(persistent.allocations.is_empty());
            LIVE.with(|n| assert_eq!(n.get(), 0));
            continue;
        }
        drop(task);
        drop(persistent);
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}

#[test]
fn persistent_probe_descriptor_keeps_dependencies_and_finishes_after_hook_failure() {
    use crate::runtime::cleanup::Hook;
    use std::{cell::Cell, rc::Rc};
    let calls = Rc::new(Cell::new(0));
    let code_owner = Rc::new(());
    let weak = Rc::downgrade(&code_owner);
    let mut task = Arena::default();
    let text = Buffer::copy_bytes(&mut task, b"owned payload").unwrap();
    let selected: Vec<_> = task.allocations.iter().map(|(p, _)| *p).collect();
    let capture = calls.clone();
    let hook = Hook::Interpreted {
        data: text.as_ptr().cast(),
        callback: Rc::new(move |data, _| {
            let _keep_alive = &code_owner;
            assert_eq!(
                unsafe { owned_text::borrow(data.cast()).unwrap() },
                "owned payload"
            );
            capture.set(capture.get() + 1);
            Err(allocation_error())
        }),
    };
    let mut cleanups = Cleanups::default();
    cleanups.insert(
        0,
        Entry {
            keep_alive: None,
            storage_size: 0,
            storage: None,
            owners: vec![Some(text.as_ptr())],
            hooks: vec![(0, hook)],
        },
    );
    let mut persistent = Arena::default();
    transfer(&mut task, &mut persistent, &selected, MAX_BYTES, None).unwrap();
    let entry = cleanups.take(0).unwrap();
    drop(cleanups);
    drop(task);
    assert!(weak.upgrade().is_some());
    assert!(unsafe { entry.run(&mut persistent) }.is_err());
    assert_eq!(calls.get(), 1);
    assert!(weak.upgrade().is_none());
    assert_eq!(persistent.bytes, 0);
    assert!(persistent.allocations.is_empty());
    LIVE.with(|n| assert_eq!(n.get(), 0));
}
