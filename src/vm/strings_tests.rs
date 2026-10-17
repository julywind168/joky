//! String ownership, escapes, and suspension survival tests.

use super::support::*;
use super::VmMode;
use crate::runtime::suspension::ResumeError;
use crate::{FunctionSignature, Type};

thread_local! {
    static OWNED_TEXT_V2: std::cell::RefCell<Option<(crate::runtime::arena::Arena, std::ptr::NonNull<crate::runtime::buffer::Buffer>)>> = const { std::cell::RefCell::new(None) };
}

unsafe extern "C" fn save_owned_text_v2(source: *const u8) -> i32 {
    let mut arena = crate::runtime::arena::Arena::default();
    let owner = crate::runtime::owned_text::from_view(&mut arena, source, 0, 1, 2);
    if let Some(owner) = std::ptr::NonNull::new(owner) {
        OWNED_TEXT_V2.with(|saved| *saved.borrow_mut() = Some((arena, owner)));
        42
    } else {
        0
    }
}

#[test]
fn owned_text_v2_runtime_copy_survives_generated_call_and_module_unload() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = super::VmEngine::with_mode(mode);
        // SAFETY: The test adapter accepts the existing borrowed String ABI,
        // copies it while live, and retains only v2 bytes in its own test arena.
        unsafe {
            vm.register_text_fn(
                "save_owned_text_v2",
                FunctionSignature::new(vec![Type::String], Type::Int32),
                save_owned_text_v2 as *const u8,
            )
            .unwrap();
        }
        vm.load_source(
            r#"extern fn save_owned_text_v2(source: String) -> Int32
            fn snapshot() -> Int32 { save_owned_text_v2("中🙂\0tail") }"#,
        )
        .unwrap();
        assert_eq!(vm.call_int32_0("snapshot").unwrap(), 42);
        vm.load_source("fn snapshot() -> Int32 { 0 }").unwrap();
        drop(vm);
        let (mut arena, owner) = OWNED_TEXT_V2
            .with(|saved| saved.borrow_mut().take())
            .unwrap();
        unsafe {
            let copy = crate::runtime::error::checked(|| {
                crate::runtime::owned_text::clone(&mut arena, owner.as_ptr(), 0, 1, 2)
            })
            .unwrap();
            crate::runtime::buffer::Buffer::release(&mut arena, owner.as_ptr()).unwrap();
            assert_eq!(
                crate::runtime::owned_text::borrow(copy).unwrap(),
                "中🙂\0tail"
            );
            crate::runtime::buffer::Buffer::release(&mut arena, copy).unwrap();
        }
        crate::runtime::arena::LIVE.with(|count| assert_eq!(count.get(), 0));
    }
}

#[test]
fn string_queries_cover_utf8_bytes_and_substrings_in_both_backends() {
    let source = r#"
        import joky/string
        fn compute() -> Int32 {
            let text = "héllo"
            let needle = "llo"
            let prefix = "hé"
            let suffix = "lo"
            let other = "héllo"
            int32(string.byte_length(&text)) * 1000000 +
                int32(string.find(&text, &needle).unwrap_or(999)) * 1000 +
                (if string.starts_with(&text, &prefix) { 100 } else { 0 }) +
                (if string.ends_with(&text, &suffix) { 10 } else { 0 }) +
                (if string.equals(&text, &other) { 1 } else { 0 })
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        assert_eq!(
            parity_vm(source, mode).call_int32_0("compute").unwrap(),
            6_003_111
        );
    }
}

#[test]
fn string_queries_keep_owned_text_alive_across_suspension() {
    let source = r#"
        import joky/string
        extern fn delayed_value(x: Int32) @suspends -> Int32
        fn compute() @suspends -> Int32 {
            let text = "héllo"
            delayed_value(7)
            let needle = "é"
            int32(string.byte_length(&text)) + int32(string.find(&text, &needle).unwrap_or(999))
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(source, mode);
        let mut task = vm.call_async("compute", &[]).unwrap();
        task.resume().unwrap();
        assert!(!task.is_finished());
        READY.with(|ready| ready.set(true));
        task.resume().unwrap();
        assert_eq!(task.cps_result(), Some(7));
        READY.with(|ready| ready.set(false));
    }
}

#[test]
fn returned_strings_and_opaque_values_survive_suspension_and_reload() {
    unsafe extern "C" fn byte_length(text: *const u8) -> i32 {
        crate::runtime::string::borrow(text).unwrap().len() as i32
    }
    let source = r#"
        import joky/vm
        extern fn delayed_value(x: Int32) @suspends -> Int32
        extern fn byte_length(text: String) -> Int32
        fn text(flag: Bool) @suspends -> String {
            if flag { return (delayed_value(1); "héllo") }
            return "no"
        }
        fn engine() -> vm.Vm { return vm.Vm.interpreter() }
        fn compute() @suspends -> Int32 {
            let handle = engine()
            let saved = text(true)
            delayed_value(2)
            vm.close(handle)
            return byte_length(saved)
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = parity_vm("", mode);
        unsafe {
            vm.register_text_fn(
                "byte_length",
                FunctionSignature::new(vec![Type::String], Type::Int32),
                byte_length as *const u8,
            )
            .unwrap();
        }
        vm.load_source(source).unwrap();
        let mut task = vm.call_async("compute", &[]).unwrap();
        for _ in 0..3 {
            task.resume().unwrap();
        }
        READY.with(|ready| ready.set(true));
        task.resume().unwrap();
        READY.with(|ready| ready.set(false));
        for _ in 0..3 {
            task.resume().unwrap();
        }
        vm.load_source("fn replacement() -> Int32 { 0 }").unwrap();
        drop(vm);
        READY.with(|ready| ready.set(true));
        task.resume().unwrap();
        assert_eq!(task.cps_result(), Some(6));
        EVENTS.with(|events| assert_eq!(*events.borrow(), [1, 2]));
    }
}

#[test]
fn mutable_strings_preserve_snapshots_across_loop_suspension() {
    unsafe extern "C" fn byte_length(text: *const u8) -> i32 {
        crate::runtime::string::borrow(text).map_or(-1, |text| text.len() as i32)
    }
    let source = r#"
        extern fn byte_length(text: String) -> Int32
        extern fn delayed_value(x: Int32) @suspends -> Int32
        fn compute() @suspends -> Int32 {
            var text: String = "first"
            let initial = text.clone()
            var i = 0
            var total = 0
            while i < 2 {
                let before = text.clone()
                text = "second"
                delayed_value(i)
                total = total + byte_length(before)
                i = i + 1
            }
            total + byte_length(initial) + byte_length(text)
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = parity_vm("", mode);
        unsafe {
            vm.register_text_fn(
                "byte_length",
                FunctionSignature::new(vec![Type::String], Type::Int32),
                byte_length as *const u8,
            )
            .unwrap();
        }
        vm.load_source(source).unwrap();
        let mut task = vm.call_async("compute", &[]).unwrap();
        drop(vm);
        READY.with(|ready| ready.set(true));
        task.resume().unwrap();
        task.resume().unwrap();
        assert!(task.is_finished());
        assert_eq!(task.cps_result(), Some(22));
    }
}

#[test]
fn escaped_strings_preserve_bytes_across_calls_suspension_and_reload() {
    unsafe extern "C" fn inspect_text(text: *const u8) -> i32 {
        match crate::runtime::string::borrow(text) {
            Ok("hé🙂{value}\n\r\t\0\"\\end") => 1,
            Ok("\\n\\r\\t\\0\\\"\\\\") => 2,
            Ok("") => 3,
            _ => -1,
        }
    }
    let mut source = include_str!("../../tests/fixtures/strings.jk").to_owned();
    source.push_str(
        r#"
        extern fn inspect_text(text: String,) -> Int32
        extern fn delayed_value(value: Int32,) @suspends -> Int32
        extern fn record(value: Int32,) -> Int32
        fn direct() -> Int32 {
            inspect_text(identity_text(escaped_text(),),) * 100 +
                inspect_text(literal_escape_text(),) * 10 + inspect_text(empty_text(),)
        }
        fn suspended() @suspends -> Int32 {
            let saved = escaped_text()
            delayed_value(record(7),)
            inspect_text(identity_text(saved,),) * 100 +
                inspect_text(literal_escape_text(),) * 10 + inspect_text(empty_text(),)
        }
    "#,
    );
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = parity_vm("", mode);
        unsafe {
            vm.register_text_fn(
                "inspect_text",
                FunctionSignature::new(vec![Type::String], Type::Int32),
                inspect_text as *const u8,
            )
            .unwrap();
        }
        vm.load_source(&source).unwrap();
        assert_eq!(vm.call_int32_0("direct").unwrap(), 123);
        let mut task = vm.call_async("suspended", &[]).unwrap();
        for _ in 0..3 {
            task.resume().unwrap();
            assert!(!task.is_finished());
        }
        EVENTS.with(|events| assert_eq!(*events.borrow(), [7, 7]));
        vm.load_source("fn replacement() {}").unwrap();
        drop(vm);
        READY.with(|ready| ready.set(true));
        task.resume().unwrap();
        assert_eq!(task.cps_result(), Some(123));
        assert_eq!(task.resume(), Err(ResumeError::AlreadyFinished));
        EVENTS.with(|events| assert_eq!(*events.borrow(), [7, 7]));
    }
}

#[test]
fn immutable_strings_survive_calls_suspension_reload_and_vm_drop() {
    unsafe extern "C" fn byte_length(text: *const u8) -> i32 {
        crate::runtime::string::borrow(text).map_or(-1, |s| s.len() as i32)
    }
    let mut vm = vm("fn placeholder() {}");
    unsafe {
        vm.register_text_fn(
            "byte_length",
            FunctionSignature::new(vec![Type::String], Type::Int32),
            byte_length as *const u8,
        )
        .unwrap();
    }
    vm.load_source(
        r#"
        extern fn byte_length(text: String) -> Int32
        extern fn delayed_value(value: Int32) @suspends -> Int32
        fn identity(text: String) -> String { text }
        fn text() -> String { "héllo {{world}}\n" }
        fn direct() -> Int32 { byte_length(identity(text())) + byte_length("") }
        fn suspended() @suspends -> Int32 {
            let saved: String = text()
            delayed_value(0)
            byte_length(identity(saved))
        }
    "#,
    )
    .unwrap();
    let expected = "héllo {world}\n".len() as i32;
    assert_eq!(vm.call_int32_0("direct").unwrap(), expected);
    let mut task = vm.call_async("suspended", &[]).unwrap();
    vm.load_source("fn replacement() {}").unwrap();
    drop(vm);
    READY.with(|r| r.set(true));
    task.resume().unwrap();
    assert_eq!(task.cps_result(), Some(expected as u64));
}

#[test]
fn owning_string_s2_basic_queries_and_root_copy() {
    let source = r#"
        fn echo(text: String) -> String { text }
        fn literal() -> String { "中🙂\0tail" }
        fn compute() -> Int32 {
            let text = "中🙂\0tail"
            let copy = text.clone()
            text.release()
            let needle = "🙂"
            let end = "tail"
            let empty = String.new()
            let repeated = "ababa"
            let first = "aba"
            let missing = "absent"
            if copy.equals(&copy) && copy.starts_with(&empty) && copy.ends_with(&end) &&
                copy.ends_with(&empty) && !copy.equals(&empty) && empty.is_empty() &&
                repeated.find(&first).unwrap_or(999) == 0 &&
                repeated.find(&empty).unwrap_or(999) == 0 &&
                repeated.find(&missing).is_none() && empty.find(&first).is_none() &&
                !empty.starts_with(&first) && !empty.ends_with(&first) {
                int32(copy.byte_length()) + int32(copy.find(&needle).unwrap_or(999))
            } else { 0 }
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(source, mode);
        assert_eq!(vm.call_int32_0("compute").unwrap(), 15);
        assert_eq!(vm.call_string_0("literal").unwrap(), "中🙂\0tail");
        let output = vm.call_string_1("echo", "中🙂\0tail").unwrap();
        drop(vm);
        assert_eq!(output, "中🙂\0tail");
        crate::runtime::arena::LIVE.with(|count| assert_eq!(count.get(), 0));
    }
}

#[test]
fn owning_string_s2_cps_capture_after_reload_and_scope_cleanup() {
    let source = r#"
        import joky/task
        fn forward(text: String) @suspends -> String {
            task.yield_now()
            text
        }
        fn echo(text: String) @suspends -> String {
            let step = forward
            let saved = step(text)
            task.yield_now()
            saved
        }
        fn forever(text: String) @suspends { while true { task.yield_now() } }
        fn ready(text: String) @suspends { text.release() }
        fn compete() @suspends -> Int32 {
            race { | forever("中🙂") | ready("done") }
            parallel { | ready("left") | ready("right") }
            42
        }
        fn unpolled() @suspends -> Int32 {
            race { | ready("done") | forever("never polled") }
            42
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = parity_vm(source, mode);
        let input = String::from("中🙂\0tail");
        let mut task = vm.call_async_string_1("echo", &input).unwrap();
        assert!(!task.is_finished());
        assert_eq!(task.take_result(), None);
        drop(input);
        vm.load_source("fn other() -> Int32 { 0 }").unwrap();
        drop(vm);
        while !task.is_finished() {
            task.resume().unwrap();
        }
        assert_eq!(task.take_result().as_deref(), Some("中🙂\0tail"));
        assert_eq!(task.take_result(), None);
        drop(task);
        crate::runtime::arena::LIVE.with(|count| assert_eq!(count.get(), 0));
        let vm = parity_vm(source, mode);
        let mut task = vm.call_async("compete", &[]).unwrap();
        while !task.is_finished() {
            task.resume().unwrap();
        }
        assert_eq!(task.cps_result(), Some(42));
        drop(task);
        crate::runtime::arena::LIVE.with(|count| assert_eq!(count.get(), 0));
        let task = vm.call_async("unpolled", &[]).unwrap();
        assert_eq!(task.cps_result(), Some(42));
        drop(task);
        crate::runtime::arena::LIVE.with(|count| assert_eq!(count.get(), 0));
        let task = vm.call_async_string_1("echo", "cancelled").unwrap();
        drop(vm);
        drop(task);
        crate::runtime::arena::LIVE.with(|count| assert_eq!(count.get(), 0));
    }
}

#[test]
fn owning_string_s2_optional_results_replacement_and_function_values() {
    let source = r#"import joky/mem

        fn optional(flag: Bool) -> Option<String> { if flag { Some("中🙂") } else { None } }
        fn fallible(flag: Bool) -> Result<String, Int32> { if flag { Ok("tail") } else { Err(7) } }
        fn measure(text: &String) -> Int32 { int32(text.byte_length()) }
        fn replace(text: &mut String, next: String) -> String { mem.replace(text, next) }
        fn compute() -> Int32 {
            var text = "first"
            let initial = text.clone()
            let old = replace(&mut text, "second")
            let size = measure
            let a = match optional(true) { Some(value) => size(&value), None => 0 }
            let b = match optional(false) { Some(value) => size(&value), None => 1 }
            let c = match fallible(true) { Ok(value) => size(&value), Err(_) => 0 }
            let d = match fallible(false) { Ok(value) => size(&value), Err(code) => code }
            a + b + c + d + size(&initial) + size(&old) + size(&text)
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(source, mode);
        assert_eq!(vm.call_int32_0("compute").unwrap(), 35);
        drop(vm);
        crate::runtime::arena::LIVE.with(|count| assert_eq!(count.get(), 0));
    }
}

#[test]
fn owning_string_s2_rejects_moves_conflicting_loans_and_private_bridges() {
    for source in [
        "fn f() { let a = \"text\" let b = a a.release() b.release() }",
        "fn f() { let a = \"text\" a.release() a.byte_length(); }",
        "fn f() { var a = \"text\" let view = &a a = \"next\" view.byte_length(); }",
        "fn f() -> &String { let a = \"text\" &a }",
        "import joky/slice as slices import joky/task fn pause() @suspends { task.yield_now() } fn f() @suspends { let a = \"text\" let view = a.bytes() parallel { | pause() } slices.len(view); }",
        "import joky/slice as slices import joky/task fn f() @suspends { let a = \"text\" let view = a.bytes() task.yield_now() slices.len(view); }",
        "fn f(a: &String) @suspends -> &String { a }",
        "import joky/string.{freeze_utf8} fn f() {}",
        "import joky/string fn f() { let text = \"text\" string.copy_text(&text); }",
        "unsafe extern \"C\" fn f(a: String)",
        "fn __joky_string_from_view_v2() {}",
        "fn __joky_user_text_v2_f() {}",
        "fn joky_string_result_free_v2() {}",
    ] {
        let error = super::support::frontend_error(source, &crate::ModuleSources::new());
        assert!(error.span().is_some(), "{source}: {error}");
    }
    // Names alone cannot grant access to trusted storage operations.
    let source = r#"fn freeze_utf8() -> String { "ordinary" } fn f() -> String { freeze_utf8() }"#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        assert_eq!(
            parity_vm(source, mode).call_string_0("f").unwrap(),
            "ordinary"
        );
    }
}

#[test]
fn owning_string_s2_budget_failures_clean_inputs_clones_and_pending_frames() {
    struct Restore(usize);
    impl Drop for Restore {
        fn drop(&mut self) {
            crate::runtime::arena::LIMIT.with(|limit| limit.set(self.0));
        }
    }
    let _restore = Restore(crate::runtime::arena::LIMIT.with(|limit| limit.get()));
    let source = r#"
        import joky/task
        fn echo(text: String) -> String { let next = text.clone() text.release() next }
        fn pending(text: String) @suspends -> String { let next = text.clone() task.yield_now() text.release() next }
        fn failure() -> String { let message = "Primary text failure" assert(false, &message) "unreachable" }
    "#;
    // Keep the fixture independent of whether the VM enables the prelude.
    let source = format!("import joky/check.{{assert}} {source}");
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(&source, mode);
        let mut failed = false;
        let mut completed = false;
        for budget in (0..1024).step_by(8) {
            crate::runtime::arena::LIMIT.with(|limit| limit.set(budget));
            match vm.call_string_1("echo", "中🙂\0tail") {
                Ok(result) => {
                    completed = true;
                    assert_eq!(result, "中🙂\0tail");
                }
                Err(error) => {
                    failed = true;
                    assert_eq!(error.code(), crate::ErrorCode::ResourceLimit);
                }
            }
            match vm.call_async_string_1("pending", "中🙂\0tail") {
                Ok(mut task) => {
                    while !task.is_finished() {
                        task.resume().unwrap();
                    }
                    assert_eq!(task.take_result().as_deref(), Some("中🙂\0tail"));
                }
                Err(error) => assert_eq!(error.code(), crate::ErrorCode::ResourceLimit),
            }
            crate::runtime::arena::LIVE
                .with(|count| assert_eq!(count.get(), 0, "{mode:?}, {budget}"));
        }
        assert!(failed && completed, "{mode:?}");
        crate::runtime::arena::LIMIT.with(|limit| limit.set(crate::runtime::arena::MAX_BYTES));
        assert_eq!(
            vm.call_string_0("failure").unwrap_err().message(),
            "Primary text failure"
        );
        crate::runtime::arena::LIMIT.with(|limit| limit.set(0));
        assert_eq!(
            vm.call_string_0("failure").unwrap_err().code(),
            crate::ErrorCode::ResourceLimit
        );
        crate::runtime::arena::LIVE.with(|count| assert_eq!(count.get(), 0));
    }
}

#[test]
fn owning_string_s2_text_registration_metadata_results_and_errors() {
    unsafe extern "C" fn echo(text: *const u8) -> *const u8 {
        text
    }
    unsafe extern "C" fn bad() -> *const u8 {
        std::ptr::null()
    }
    unsafe extern "C" fn pending(frame: *mut crate::runtime::cps::CpsFrame) -> i32 {
        let frame = unsafe { &mut *frame };
        assert_eq!(frame.slots.len(), 3);
        assert_eq!(
            unsafe { crate::runtime::string::borrow(frame.slots[0] as *const u8) }.unwrap(),
            "中🙂\0tail"
        );
        if frame.state == 0 {
            frame.slots[2] = 100;
        }
        frame.state += 1;
        if frame.state < 3 {
            return 1;
        }
        assert_eq!(frame.slots[2], 100);
        frame.result = frame.slots[0];
        0
    }
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = parity_vm("", mode);
        let signature = FunctionSignature::new(vec![Type::String], Type::String);
        let owner = std::rc::Rc::new(());
        unsafe {
            assert_eq!(
                vm.register_fn("echo", signature.clone(), echo as *const u8)
                    .unwrap_err()
                    .code(),
                crate::ErrorCode::AbiError
            );
            assert!(vm
                .register_text_fn(
                    "plain",
                    FunctionSignature::new(vec![], Type::Int32),
                    bad as *const u8
                )
                .is_err());
            assert!(vm
                .register_owned_text_suspending_fn(
                    "pending",
                    signature.clone(),
                    pending,
                    Some(0),
                    None
                )
                .is_err());
            vm.register_text_fn("echo", signature.clone(), echo as *const u8)
                .unwrap();
            vm.register_text_fn(
                "bad",
                FunctionSignature::new(vec![], Type::String),
                bad as *const u8,
            )
            .unwrap();
            vm.register_owned_text_suspending_fn(
                "pending",
                signature,
                pending,
                Some(3),
                Some(owner.clone()),
            )
            .unwrap();
        }
        vm.load_source(
            r#"
            extern fn echo(text: String) -> String
            extern fn bad() -> String
            extern fn pending(text: String) @suspends -> String
            fn direct(text: String) -> String { echo(text) }
            fn invalid() -> String { bad() }
            fn suspended(text: String) @suspends -> String { pending(text) }
        "#,
        )
        .unwrap();
        assert_eq!(
            vm.function_entry_abi("direct"),
            Some(crate::EntryAbi::OwnedTextV2)
        );
        assert!(vm.call_int32_0("direct").is_err());
        assert_eq!(
            vm.call_string_1("direct", "中🙂\0tail").unwrap(),
            "中🙂\0tail"
        );
        assert_eq!(
            vm.call_string_0("invalid").unwrap_err().code(),
            crate::ErrorCode::AbiError
        );
        let mut task = vm.call_async_string_1("suspended", "中🙂\0tail").unwrap();
        vm.load_source("fn replacement() {}").unwrap();
        drop(vm);
        assert!(std::rc::Rc::strong_count(&owner) > 1);
        while !task.is_finished() {
            task.resume().unwrap();
        }
        assert_eq!(task.take_result().as_deref(), Some("中🙂\0tail"));
        drop(task);
        assert_eq!(std::rc::Rc::strong_count(&owner), 1);
        crate::runtime::arena::LIVE.with(|count| assert_eq!(count.get(), 0));
    }
}

#[test]
fn owning_string_s2_root_cleanup_failure_discards_text() {
    let source = r#"
        import joky/array.{type Array}
        import joky/drop.{type Drop}
        import joky/check.{assert}
        struct Resource { data: Array<UInt8> }
        impl Drop for Resource {
            fn drop(&mut self) { let message = "Text cleanup failure" assert(false, &message) }
        }
        fn text() -> String { let resource = Resource { data: Array.new::<UInt8>() } "discarded" }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(source, mode);
        assert_eq!(
            vm.call_string_0("text").unwrap_err().message(),
            "Text cleanup failure"
        );
        crate::runtime::arena::LIVE.with(|count| assert_eq!(count.get(), 0));
    }
}
