use super::*;
use crate::core::types::{memory_layout::MAX_DEPTH, ConstArgument, FixedArrayType};
use crate::runtime::{arena, buffer::Buffer, cleanup, error, explicit_arena, float_env};
use crate::{ErrorCode, Type};
use std::sync::Arc;

// Available in the macOS pthread ABI, but not exposed by our libc version.
unsafe extern "C" {
    fn pthread_attr_setstack(
        attr: *mut libc::pthread_attr_t,
        address: *mut libc::c_void,
        size: usize,
    ) -> libc::c_int;
}

fn nested_buffers(arena: &mut arena::Arena) -> *mut Buffer {
    let mut owner = Buffer::copy_bytes(arena, b"leaf").unwrap();
    for _ in 0..1024 {
        let mut parent = Buffer::allocate(arena, 1, 8, 8).unwrap();
        unsafe {
            parent
                .as_mut()
                .configure(crate::runtime::buffer::ElementCleanup { owners: vec![0] })
                .unwrap();
            parent
                .as_ref()
                .initialize_next()
                .unwrap()
                .cast::<*mut Buffer>()
                .write(owner.as_ptr());
            parent.as_mut().commit_next();
        }
        owner = parent;
    }
    owner.as_ptr()
}

unsafe fn exercise(nodes: &[crate::runtime::place::CopyNode]) {
    let sp: usize;
    std::arch::asm!("mov {}, sp", out(reg) sp, options(nomem, nostack));
    // First admission in this pthread: cold TLS and pthread boundary lookup.
    assert_eq!(room(sp, 0, 7, 11, 19), 0);
    // Verify the OS did not silently grant a larger stack than the budget.
    let thread = libc::pthread_self();
    let size = libc::pthread_get_stacksize_np(thread);
    assert_eq!(size, EMERGENCY_BUDGET);
    let low = libc::pthread_get_stackaddr_np(thread) as usize - size;
    assert!(sp - low <= EMERGENCY_BUDGET);
    assert_eq!(error::joky_runtime_error_start(), 11);
    error::joky_runtime_error_clear();

    let mut arena = arena::Arena::default();
    let frame = cleanup::new(&mut arena);
    let register = |slot, owner| {
        (*frame).entries.insert(
            slot,
            cleanup::Entry {
                keep_alive: None,
                storage_size: 0,
                storage: None,
                owners: [Some(owner)].into(),
            },
        );
    };
    // Exercise both the inline cleanup entry and BTreeMap-backed registrations.
    for slot in 0..64 {
        register(
            slot,
            Buffer::copy_bytes(&mut arena, b"text").unwrap().as_ptr(),
        );
    }
    register(64, nested_buffers(&mut arena));
    // Known runtime-owned resource callbacks, never arbitrary host release.
    let region = explicit_arena::new(&mut arena).unwrap();
    explicit_arena::allocate(region.as_ptr(), 4096, 8).unwrap();
    register(65, region.as_ptr());
    register(
        66,
        crate::runtime::buffer::crypto::stream_new(&mut arena, None)
            .unwrap()
            .as_ptr(),
    );
    let key = Buffer::copy_bytes(&mut arena, b"key").unwrap();
    register(
        67,
        crate::runtime::buffer::crypto::stream_new(&mut arena, Some(key.as_ref()))
            .unwrap()
            .as_ptr(),
    );
    register(68, key.as_ptr());

    // A maximum-depth partially initialized place: one live and one zero lane.
    let pointer = (*frame)
        .entries
        .places
        .begin(&mut arena, 69, 16, 8, 2)
        .unwrap();
    (*frame)
        .entries
        .places
        .describe(&mut arena, 69, nodes)
        .unwrap();
    pointer
        .as_ptr()
        .cast::<*mut Buffer>()
        .write(Buffer::copy_bytes(&mut arena, b"partial").unwrap().as_ptr());
    (*frame).entries.places.commit_next(69).unwrap();

    let _restore = float_env::Guard::enter();
    float_env::testing::hostile();
    let state = float_env::testing::state();
    let saved = float_env::enter();
    let limit = arena::LIMIT.with(|limit| limit.replace(0));
    assert_eq!(room(sp, 0, 7, 21, 29), 0);
    for site in 0..10_000 {
        error::call_failed(7, site, site + 1);
    }
    cleanup::finish(frame);
    // Ordinary arena deallocation is part of the constrained exit path too.
    drop(arena);
    float_env::restore(saved);
    arena::LIMIT.with(|value| value.set(limit));
    assert_eq!(float_env::testing::state(), state);
    arena::LIVE.with(|live| assert_eq!(live.get(), 0));
    // Materialize only after cleanup and unwinding the tested operation.
    let failure = error::current_error().unwrap();
    assert_eq!(failure.code(), ErrorCode::ResourceLimit);
    assert_eq!(
        failure.span(),
        Some(Span::new(21, 29).with_source(SourceId::from_raw(7)))
    );
    assert_eq!(failure.native_call_sites().len(), 1);
    assert!(failure.help()[0].contains("9999"));
    error::joky_runtime_error_clear();
}

#[test]
fn native_stack_emergency_budget_covers_local_cleanup() {
    const CHILD: &str = "JOKY_EMERGENCY_BUDGET_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let name = std::thread::current().name().unwrap().to_owned();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &name, "--nocapture"])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        return;
    }
    let mut ty = Type::String;
    for depth in 0..MAX_DEPTH {
        ty = Type::FixedArray(Arc::new(FixedArrayType {
            element: ty,
            length: ConstArgument::Value(if depth == 0 { 2 } else { 1 }),
        }));
    }
    let nodes = ty.place_copy_program();
    struct Context {
        nodes: Vec<crate::runtime::place::CopyNode>,
        passed: bool,
    }
    extern "C" fn run(pointer: *mut libc::c_void) -> *mut libc::c_void {
        let context = unsafe { &mut *pointer.cast::<Context>() };
        context.passed = std::panic::catch_unwind(|| {
            for _ in 0..2 {
                unsafe { exercise(&context.nodes) };
            }
        })
        .is_ok();
        std::ptr::null_mut()
    }
    let mut context = Context {
        nodes,
        passed: false,
    };
    // macOS can enlarge a requested pthread stack. Supply an exact mapping with
    // a guard page so the OS cannot silently give this test extra headroom.
    unsafe {
        let page = libc::sysconf(libc::_SC_PAGESIZE);
        assert!(page > 0);
        let page = page as usize;
        let allocation = libc::mmap(
            std::ptr::null_mut(),
            EMERGENCY_BUDGET + page,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANON,
            -1,
            0,
        );
        assert_ne!(allocation, libc::MAP_FAILED);
        assert_eq!(libc::mprotect(allocation, page, libc::PROT_NONE), 0);
        let mut attr = std::mem::MaybeUninit::uninit();
        assert_eq!(libc::pthread_attr_init(attr.as_mut_ptr()), 0);
        let mut attr = attr.assume_init();
        assert_eq!(
            pthread_attr_setstack(&mut attr, allocation.byte_add(page), EMERGENCY_BUDGET),
            0
        );
        let mut thread = std::mem::MaybeUninit::uninit();
        let status = libc::pthread_create(
            thread.as_mut_ptr(),
            &attr,
            run,
            (&mut context as *mut Context).cast(),
        );
        assert_eq!(libc::pthread_attr_destroy(&mut attr), 0);
        assert_eq!(status, 0);
        assert_eq!(
            libc::pthread_join(thread.assume_init(), std::ptr::null_mut()),
            0
        );
        assert_eq!(libc::munmap(allocation, EMERGENCY_BUDGET + page), 0);
    }
    assert!(context.passed, "constrained pthread failed");
}
