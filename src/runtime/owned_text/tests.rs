use super::*;
use crate::runtime::{arena, error, string};
use std::cell::Cell;

struct RestoreLimit(usize);
impl Drop for RestoreLimit {
    fn drop(&mut self) {
        arena::LIMIT.with(|limit| limit.set(self.0));
    }
}

#[test]
fn owned_text_v2_copies_literals_and_clones_independent_byte_owners() {
    for text in ["", "ASCII", "中🙂\0tail", "e\u{301}"] {
        let mut arena = Arena::default();
        let encoded = string::encode(text);
        let owner =
            error::checked(|| unsafe { literal(&mut arena, encoded.as_ptr(), 0, 1, 2) }).unwrap();
        let copy = error::checked(|| unsafe { clone(&mut arena, owner, 0, 3, 4) }).unwrap();
        assert_ne!(owner, copy);
        drop(encoded);
        unsafe {
            assert_eq!(borrow(owner).unwrap(), text);
            Buffer::release(&mut arena, owner).unwrap();
            assert_eq!(borrow(copy).unwrap(), text);
            assert_eq!((*copy).len(), text.len() as u64);
            Buffer::release(&mut arena, copy).unwrap();
        }
        arena::LIVE.with(|count| assert_eq!(count.get(), 0));
    }
}

#[test]
fn owned_text_v2_budget_failures_reclaim_partial_allocations_and_preserve_sources() {
    let _restore = RestoreLimit(arena::LIMIT.with(Cell::get));
    let encoded = string::encode("中🙂\0tail");
    let mut failures = 0;
    let mut successes = 0;
    for budget in 0..512 {
        let mut task = Arena::default();
        arena::LIMIT.with(|limit| limit.set(budget));
        match error::checked(|| unsafe { literal(&mut task, encoded.as_ptr(), 0, 7, 9) }) {
            Ok(owner) => {
                assert_eq!(unsafe { borrow(owner) }.unwrap(), "中🙂\0tail");
                arena::LIMIT.with(|limit| limit.set(0));
                let failure =
                    error::checked(|| unsafe { clone(&mut task, owner, 0, 11, 13) }).unwrap_err();
                assert_eq!(failure.code(), ErrorCode::ResourceLimit);
                assert_eq!(unsafe { borrow(owner) }.unwrap(), "中🙂\0tail");
                unsafe {
                    Buffer::release(&mut task, owner).unwrap();
                }
                successes += 1;
            }
            Err(failure) => {
                assert_eq!(failure.code(), ErrorCode::ResourceLimit);
                assert_eq!(failure.span().unwrap(), Span::new(7, 9));
                failures += 1;
            }
        }
        drop(task);
        arena::LIVE.with(|count| assert_eq!(count.get(), 0, "budget={budget}"));
    }
    assert!(failures > 0 && successes > 0);
}

#[test]
fn owned_text_v2_rejects_invalid_sources_and_nonbyte_storage() {
    let mut task = Arena::default();
    let oversized = (MAX_BYTES as u64 + 1).to_ne_bytes();
    let error =
        error::checked(|| unsafe { literal(&mut task, oversized.as_ptr(), 0, 1, 2) }).unwrap_err();
    assert_eq!(error.code(), ErrorCode::ResourceLimit);
    let invalid = [1u64.to_ne_bytes().as_slice(), &[0xff]].concat();
    let error =
        error::checked(|| unsafe { literal(&mut task, invalid.as_ptr(), 0, 1, 2) }).unwrap_err();
    assert_eq!(error.code(), ErrorCode::AbiError);
    assert!(error::checked(|| unsafe { literal(&mut task, std::ptr::null(), 0, 1, 2) }).is_err());
    assert!(error::checked(|| unsafe { clone(&mut task, std::ptr::null(), 0, 1, 2) }).is_err());
    let invalid_owner = Buffer::copy_bytes(&mut task, &[0xff]).unwrap();
    let nonbyte = Buffer::allocate(&mut task, 0, 4, 4).unwrap();
    unsafe {
        assert!(borrow(invalid_owner.as_ptr()).is_err());
        assert!(borrow(nonbyte.as_ptr()).is_err());
        Buffer::release(&mut task, invalid_owner.as_ptr()).unwrap();
        Buffer::release(&mut task, nonbyte.as_ptr()).unwrap();
    }
    arena::LIVE.with(|count| assert_eq!(count.get(), 0));
}

#[test]
fn owned_text_v2_error_messages_survive_release_and_exhausted_task_budget() {
    let _restore = RestoreLimit(arena::LIMIT.with(Cell::get));
    let mut task = Arena::default();
    let encoded = string::encode("primary 中🙂\0tail");
    let message =
        error::checked(|| unsafe { literal(&mut task, encoded.as_ptr(), 0, 1, 2) }).unwrap();
    arena::LIMIT.with(|limit| limit.set(0));
    let failure = error::checked(|| unsafe {
        error::require_failed_v2(message, 0, 5, 8);
        // Subsequent failing allocation must not replace the require message.
        assert!(clone(&mut task, message, 0, 10, 12).is_null());
        Buffer::release(&mut task, message).unwrap();
    })
    .unwrap_err();
    drop(task);
    assert_eq!(failure.code(), ErrorCode::RuntimeError);
    assert_eq!(failure.message(), "primary 中🙂\0tail");
    assert_eq!(failure.span().unwrap(), Span::new(5, 8));
    arena::LIVE.with(|count| assert_eq!(count.get(), 0));

    let mut task = Arena::default();
    let failure = error::checked(|| unsafe {
        assert!(literal(&mut task, encoded.as_ptr(), 0, 13, 17).is_null());
        error::require_failed_v2(std::ptr::null(), 0, 20, 24);
    })
    .unwrap_err();
    assert_eq!(failure.code(), ErrorCode::ResourceLimit);
    assert_eq!(failure.span().unwrap(), Span::new(13, 17));
}
