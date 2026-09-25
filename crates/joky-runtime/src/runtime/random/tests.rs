use super::*;
use crate::runtime::bytes::{jk_bytes_data, jk_bytes_length};
use crate::runtime::continuation::{
    jk_continuation_alloc_suspend_result, jk_continuation_cancel, jk_continuation_free,
    jk_continuation_new, jk_continuation_start_suspend_payload, jk_continuation_state,
    jk_continuation_suspend_result_pointer, ContinuationState,
};
use crate::runtime::provider::ProviderOperationEntry;
use crate::runtime::scope::RuntimeScope;
use std::cell::RefCell;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[test]
fn generation_validates_before_allocation_or_entropy_and_preserves_binary_bytes() {
    for length in [MAX_LENGTH + 1, u64::MAX] {
        assert!(generate(length, |_| -> Result<(), &str> { panic!("invalid length") }).is_err());
    }
    assert!(
        generate(0, |_| -> Result<(), &str> { panic!("empty request") })
            .unwrap()
            .is_empty()
    );
    for length in [1, 24, 256, MAX_LENGTH] {
        let data = generate(length, |data| {
            assert_eq!(data.len(), length as usize);
            for (i, byte) in data.iter_mut().enumerate() {
                *byte = i as u8;
            }
            Ok::<_, &str>(())
        })
        .unwrap();
        assert_eq!(data.len(), length as usize);
        assert!(data.iter().enumerate().all(|(i, &byte)| byte == i as u8));
    }
}

#[test]
fn entropy_failure_discards_partial_output_without_fallback() {
    let result = generate(32, |data| {
        data[..16].fill(0xab);
        Err("injected source failure")
    });
    assert_eq!(
        result.unwrap_err(),
        "random: OS random source failed: injected source failure"
    );
}

#[test]
fn hook_rejects_invalid_layouts_and_dead_continuations() {
    let scope = RuntimeScope::new();
    let _guard = scope.enter();
    let handle = jk_continuation_new(1);
    let length = 24u64;
    unsafe {
        for (arguments, size, result_size) in [
            (std::ptr::null(), 8, 4 * WORD),
            ((&length as *const u64).cast(), 0, 4 * WORD),
            ((&length as *const u64).cast(), 7, 4 * WORD),
            ((&length as *const u64).cast(), 16, 4 * WORD),
            ((&length as *const u64).cast(), 8, 3 * WORD),
            ((&length as *const u64).cast(), 8, 5 * WORD),
        ] {
            assert_eq!(
                bytes_start(
                    handle,
                    0,
                    arguments,
                    size,
                    std::ptr::null_mut(),
                    result_size
                ),
                0
            );
        }
        jk_continuation_free(handle);
        assert_eq!(
            bytes_start(
                handle,
                0,
                (&length as *const u64).cast(),
                8,
                std::ptr::null_mut(),
                4 * WORD
            ),
            0
        );
    }
    assert_eq!(scope.managed_objects.load(Ordering::Acquire), 0);
}

#[test]
fn registered_provider_transfers_exact_bytes_and_business_errors() {
    let scope = RuntimeScope::new();
    let _guard = scope.enter();
    const OP: u64 = 0xfefe_0031;
    let _registration = register_operations(&[ProviderOperationEntry {
        effect: "random",
        name: "bytes",
        operation: OP,
    }])
    .unwrap();
    for length in [0u64, 24, MAX_LENGTH, MAX_LENGTH + 1, u64::MAX] {
        let handle = jk_continuation_new(2);
        unsafe {
            assert!(!jk_continuation_alloc_suspend_result(handle, 4 * WORD).is_null());
        }
        let arguments = length.to_ne_bytes();
        unsafe {
            assert_eq!(
                jk_continuation_start_suspend_payload(
                    handle,
                    OP,
                    arguments.as_ptr(),
                    arguments.len()
                ),
                1
            );
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while unsafe { jk_continuation_state(handle) } != ContinuationState::Ready as u8 {
            assert!(
                Instant::now() < deadline,
                "random provider failed to complete"
            );
            std::thread::yield_now();
        }
        unsafe {
            let result = jk_continuation_suspend_result_pointer(handle).cast::<usize>();
            let words = std::ptr::read_unaligned(result.cast::<[usize; 4]>());
            let owned = if length <= MAX_LENGTH {
                assert_eq!(words[0], 0);
                let pointer = words[1] as *mut u8;
                assert_eq!(jk_bytes_length(pointer), length as usize);
                assert!(!jk_bytes_data(pointer).is_null());
                pointer
            } else {
                assert_eq!(words[0], 1);
                let error = std::slice::from_raw_parts(words[2] as *const u8, words[3]);
                assert_eq!(error, b"random: length exceeds the 1048576-byte limit");
                words[2] as *mut u8
            };
            std::ptr::write_bytes(result, 0, 4);
            jk_drop(owned);
            jk_continuation_free(handle);
        }
    }
    scope.wait_for_idle();
    assert_eq!(scope.managed_objects.load(Ordering::Acquire), 0);
}

struct OnDrop(mpsc::Sender<()>);
impl Drop for OnDrop {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

type Generator = Box<dyn FnOnce() -> Result<Vec<u8>, String> + Send>;
thread_local! {
    static GENERATOR: RefCell<Option<Generator>> = const { RefCell::new(None) };
}

unsafe extern "C" fn controlled_start(
    handle: *mut Continuation,
    operation: u64,
    _arguments: *const u8,
    _size: usize,
    _result: *mut u8,
    _result_size: usize,
) -> u8 {
    // Installation and start occur on the test thread; generation runs in the pool.
    let generator = GENERATOR.with_borrow_mut(|value| value.take().unwrap());
    start(handle, operation, generator)
}

#[test]
fn cancellation_before_admission_drops_captures_without_reading_entropy() {
    let scope = RuntimeScope::new();
    let _guard = scope.enter();
    let handle = jk_continuation_new(3);
    unsafe {
        assert_eq!(jk_continuation_cancel(handle), 1);
    }
    let (tx, rx) = mpsc::channel();
    let capture = OnDrop(tx);
    assert_eq!(
        start(handle, 0, move || {
            let _capture = capture;
            panic!("cancelled operation ran");
        }),
        1
    );
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
    unsafe {
        jk_continuation_free(handle);
    }
    scope.wait_for_idle();
    assert_eq!(scope.managed_objects.load(Ordering::Acquire), 0);
}

#[test]
fn running_cancellation_releases_late_bytes_and_errors_in_requesting_scope() {
    for result in [
        Ok(vec![]),
        Ok(vec![0, 255, 1, 128]),
        Err("late entropy failure".into()),
    ] {
        let scope = RuntimeScope::new();
        let _guard = scope.enter();
        let handle = jk_continuation_new(4);
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        GENERATOR.with_borrow_mut(|generator| {
            *generator = Some(Box::new(move || {
                started_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                result
            }));
        });
        const OP: u64 = 0xfefe_0032;
        let _registration = crate::runtime::provider::register_named(
            &[("bytes", controlled_start)],
            cancel,
            &[ProviderOperationEntry {
                effect: "random",
                name: "bytes",
                operation: OP,
            }],
        )
        .unwrap();
        unsafe {
            assert!(!jk_continuation_alloc_suspend_result(handle, 4 * WORD).is_null());
            assert_eq!(
                jk_continuation_start_suspend_payload(handle, OP, std::ptr::null(), 0),
                1
            );
        }
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        unsafe {
            assert_eq!(jk_continuation_cancel(handle), 1);
            jk_continuation_free(handle);
        }
        release_tx.send(()).unwrap();
        // The provider's cleanup lease outlives cancellation and payload disposal.
        scope.wait_for_idle();
        assert_eq!(scope.managed_objects.load(Ordering::Acquire), 0);
    }
}
