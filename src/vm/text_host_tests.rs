//! Explicit host descriptors, bounded live storage and wide CPS signatures.
use super::{VmEngine, VmMode};
use crate::runtime::{arena, cps::CpsFrame, string};
use crate::{FunctionSignature, Type};

struct RestoreLimit(usize);
impl Drop for RestoreLimit {
    fn drop(&mut self) {
        arena::LIMIT.with(|limit| limit.set(self.0));
    }
}
unsafe extern "C" fn inspect(text: *const u8) -> i32 {
    string::borrow(text).map_or(-1, |text| text.len() as i32)
}
unsafe extern "C" fn echo(text: *const u8) -> *const u8 {
    text
}
unsafe extern "C" fn delayed_echo(frame: *mut CpsFrame) -> i32 {
    let frame = &mut *frame;
    if frame.state < 2 {
        frame.state += 1;
        return 1;
    }
    frame.result = frame.slots[0];
    0
}

#[test]
fn text_host_repeated_calls_reuse_budget_and_pending_descriptors() {
    let _restore = RestoreLimit(arena::LIMIT.with(|limit| limit.get()));
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        // SAFETY: Exact descriptor ABI, no retained sync borrows or unwinding.
        unsafe {
            vm.register_text_fn(
                "inspect",
                FunctionSignature::new(vec![Type::reference(Type::String, false)], Type::Int32),
                inspect as *const u8,
            )
            .unwrap();
            vm.register_text_fn(
                "echo",
                FunctionSignature::new(vec![Type::String], Type::String),
                echo as *const u8,
            )
            .unwrap();
            vm.register_text_suspending_fn(
                "delayed_echo",
                FunctionSignature::new(vec![Type::String], Type::String),
                delayed_echo,
            )
            .unwrap();
        }
        vm.load_source(crate::text_host_model::LOOPS).unwrap();
        let input = "中🙂\0text".repeat(100);
        arena::LIMIT.with(|limit| limit.set(8192));
        for entry in ["borrowed", "copied"] {
            assert_eq!(
                vm.call_string_1(entry, &input).unwrap(),
                input,
                "{mode:?} {entry}"
            );
            arena::LIVE.with(|live| assert_eq!(live.get(), 0));
        }
        let cancelled = vm.call_async_string_1("pending", &input).unwrap();
        assert!(!cancelled.is_finished());
        drop(cancelled);
        arena::LIVE.with(|live| assert_eq!(live.get(), 0));
        let mut task = vm.call_async_string_1("pending", &input).unwrap();
        vm.load_source("fn replacement() {}").unwrap();
        drop(vm);
        while !task.is_finished() {
            task.resume().unwrap();
        }
        assert_eq!(task.take_result().as_deref(), Some(input.as_str()));
        drop(task);
        arena::LIVE.with(|live| assert_eq!(live.get(), 0));
        arena::LIMIT.with(|limit| limit.set(arena::MAX_BYTES));
    }
}

unsafe extern "C" fn wide(frame: *mut CpsFrame) -> i32 {
    let frame = &mut *frame;
    if string::borrow(frame.slots[1] as *const u8).ok() != Some("left")
        || string::borrow(frame.slots[65] as *const u8).ok() != Some("中🙂\0tail")
        || frame.slots[64] != 64
        || frame.slots[66] != 66
    {
        return 2;
    }
    if frame.state == 0 {
        frame.state = 1;
        return 1;
    }
    frame.result = frame.slots[65];
    0
}
#[test]
fn text_host_cps_parameters_beyond_sixty_four_keep_slots_and_owners() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        let mut params = vec![Type::Int32; 67];
        params[0] = Type::Unit;
        params[1] = Type::String;
        params[65] = Type::String;
        // SAFETY: The callback uses the declared slots and only returns live views.
        unsafe {
            vm.register_owned_text_suspending_fn(
                "wide",
                FunctionSignature::new(params, Type::String),
                wide,
                Some(67),
                None,
            )
            .unwrap();
        }
        vm.load_source(&crate::text_host_model::wide_source())
            .unwrap();
        let mut task = vm.call_async_string_0("run").unwrap();
        assert!(!task.is_finished());
        vm.load_source("fn replacement() {}").unwrap();
        drop(vm);
        task.resume().unwrap();
        assert_eq!(task.take_result().as_deref(), Some("中🙂\0tail"));
        drop(task);
        arena::LIVE.with(|live| assert_eq!(live.get(), 0));
    }
}

#[test]
fn text_borrows_cannot_cross_parallel_or_race() {
    for boundary in ["parallel", "race"] {
        let source = format!("import joky/task fn pause() @suspends {{ task.yield_now() }} fn run() @suspends -> UInt64 {{ let text = \"hello\" let view = &text {boundary} {{ | pause() }} view.byte_length() }}");
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let error = VmEngine::with_mode(mode).load_source(&source).unwrap_err();
            assert_eq!(error.code(), crate::ErrorCode::InvalidCall);
            assert_eq!(
                error.message(),
                "A borrowed text view cannot remain live across suspension"
            );
        }
        let error = crate::aot::AotCompiler::new("borrow-boundary")
            .unwrap()
            .compile_module(&crate::parser::parse(&source).unwrap())
            .unwrap_err();
        assert_eq!(
            error.message(),
            "A borrowed text view cannot remain live across suspension"
        );
    }
}
