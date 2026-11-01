//! Descriptor reuse and escape fallback execute through both VM backends.
use super::{support::*, VmMode};
use crate::runtime::arena::{LIMIT, LIVE, LOCAL_LIMIT};

const SOURCES: [&str; 2] = [
    include_str!("../../../../tests/fixtures/local_slice_descriptors.jk"),
    include_str!("../../../../tests/fixtures/local_fixed_slice_descriptors.jk"),
];

fn engines() -> impl Iterator<Item = (&'static str, VmMode)> {
    SOURCES
        .into_iter()
        .flat_map(|source| [VmMode::Jit, VmMode::Interpreter].map(|mode| (source, mode)))
}

fn clean() {
    LIVE.with(|n| assert_eq!(n.get(), 0));
}

struct Restore(usize, usize);
impl Drop for Restore {
    fn drop(&mut self) {
        LIMIT.with(|n| n.set(self.0));
        LOCAL_LIMIT.with(|n| n.set(self.1));
    }
}

#[test]
fn local_slice_descriptors_bound_loops_and_preserve_escaping_views() {
    let _restore = Restore(LIMIT.with(|n| n.get()), LOCAL_LIMIT.with(|n| n.get()));
    for (source, mode) in engines() {
        let vm = parity_vm(source, mode);
        // Enough for fixed owners/locals, far below 512 descriptor allocations.
        LIMIT.with(|n| n.set(4096));
        LOCAL_LIMIT.with(|n| n.set(4096));
        for entry in [
            "descriptor_loop",
            "descriptor_previous",
            "descriptor_returned",
        ] {
            assert_eq!(vm.call_int32_0(entry).unwrap(), 42, "{mode:?}: {entry}");
            clean();
        }
    }
}

#[test]
fn local_slice_descriptors_survive_suspend_and_release_on_every_exit() {
    for (source, mode) in engines() {
        for entry in [
            "descriptor_suspend",
            "descriptor_failure",
            "descriptor_bounds",
        ] {
            for cancel in [false, true] {
                let vm = parity_vm(source, mode);
                let mut task = vm.call_async(entry, &[]).unwrap();
                let live = LIVE.with(|n| n.get());
                assert!(live > 0);
                for _ in 0..3 {
                    task.resume().unwrap();
                    assert!(!task.is_finished());
                    LIVE.with(|n| assert_eq!(n.get(), live));
                }
                if cancel {
                    drop(task);
                } else {
                    READY.with(|n| n.set(true));
                    let mut failed = false;
                    for _ in 0..16 {
                        if task.is_finished() {
                            break;
                        }
                        failed |= task.resume().is_err();
                    }
                    assert!(task.is_finished());
                    if entry == "descriptor_suspend" {
                        assert!(!failed);
                        assert_eq!(task.cps_result(), Some(42));
                    } else {
                        assert!(failed);
                    }
                }
                clean();
            }
        }
    }
}

#[test]
fn local_slice_descriptors_local_allocation_failure_cleans_owners() {
    let _restore = Restore(LIMIT.with(|n| n.get()), LOCAL_LIMIT.with(|n| n.get()));
    for source in SOURCES {
        let vm = parity_vm(source, VmMode::Interpreter);
        LOCAL_LIMIT.with(|n| n.set(0));
        let error = vm.call_int32_0("descriptor_loop").unwrap_err();
        assert_eq!(error.code(), crate::ErrorCode::ResourceLimit);
        clean();
    }
}

fn cancellation(sweep: CancellationSweep) {
    for source in SOURCES {
        let source = format!(
            "{}\nfn descriptor_cancel() @suspends -> Int32 {{ descriptor_loop() }}",
            source.replace("512", "3"),
        );
        let mut vm = parity_vm(&source, VmMode::Interpreter);
        assert_cancellation(
            &mut vm,
            "descriptor_cancel",
            100_000,
            sweep,
            || {},
            || LIVE.with(|n| n.get()),
            clean,
        );
    }
}

#[test]
fn local_slice_descriptors_cancel_at_storage_boundaries() {
    cancellation(CancellationSweep::ResourceBoundaries);
}

#[test]
#[ignore = "exhaustive: instruction-by-instruction descriptor cancellation"]
fn exhaustive_local_slice_descriptors_cancellation() {
    cancellation(CancellationSweep::EveryInstruction);
}
