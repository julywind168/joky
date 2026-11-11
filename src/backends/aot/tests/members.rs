//! Members AOT fixtures.
use super::*;

#[test]
#[cfg(unix)]
fn inherent_method_native_objects_preserve_direct_and_cps_calls() {
    for suspends in [false, true] {
        let source = with_suspends(
            include_str!("../../../../tests/fixtures/methods.jk"),
            suspends,
            Suspends::Typed,
        );
        let fixture = ScratchDir::new("methods");
        let dir = fixture.path();
        let object = dir.join("methods.o");
        let program = dir.join("methods");
        let host = dir.join("host.c");
        let mut compiler = AotCompiler::new("methods").unwrap();
        compiler
            .compile_module(&crate::parser::parse(&source).unwrap())
            .unwrap();
        let harness = if suspends {
            let check = compiler.cps_frame_slots("check").unwrap();
            let wide = compiler.cps_frame_slots("wide").unwrap();
            format!(
                r#"
                #include "joky_cps.h"
                extern int32_t check(JokyCpsFrame *), wide(JokyCpsFrame *);
                int main(void) {{
                    JokyCpsFrame *c = joky_cps_frame_new({check});
                    if (check(c) != 0 || joky_cps_result(c) != 42) return 1;
                    joky_cps_frame_free(c);
                    c = joky_cps_frame_new({wide});
                    if (wide(c) != 0 || joky_cps_result(c) != UINT64_MAX) return 2;
                    joky_cps_frame_free(c);
                    return 0;
                }}
            "#
            )
        } else {
            "#include <stdint.h>\nextern int32_t check(void); extern uint64_t wide(void);\nint main(void) { return check() != 42 || wide() != UINT64_MAX; }".to_owned()
        };
        compiler.write_object(&object).unwrap();
        std::fs::write(&host, harness).unwrap();
        link(
            link_runtime(
                Command::new("cc")
                    .arg(&host)
                    .arg(&object)
                    .args(["-Wall", "-Wextra", "-Werror", "-I"])
                    .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("include")),
            ),
            &program,
        );
        let output = Command::new(program).output().unwrap();
        assert!(output.status.success(), "suspends={suspends}: {output:?}");
    }
}

#[test]
#[cfg(unix)]
fn method_native_pending_arguments_keep_snapshots_and_do_not_replay_effects() {
    let source = r#"
        import joky/integer
        extern fn record(n: Int32) -> Int32
        extern fn delay(n: Int32) @suspends -> Int32
        struct Pair { x: Int32 }
        impl Pair {
            fn add(self, n: Int32) @suspends -> Pair { Pair { x: self.x + delay(n) } }
        }
        trait PendingOps {
            fn receive(self, other: Int32) @suspends -> Option<UInt64>
            fn branch_task(self) @suspends
        }
        impl PendingOps for Int32 {
            fn receive(self, other: Int32) @suspends -> Option<UInt64> { delay(other); self.to_u64() }
            fn branch_task(self) @suspends { record(delay(self)); }
        }
        fn pending() @suspends -> Int32 {
            var pair = Pair { x: record(40) }
            let result = pair.add({ pair.x = 99; record(2) })
            result.x + pair.x
        }
        fn receiver() @suspends -> Int32 {
            match delay(42).receive(record(7)) { Some(n) => int32(n), None => -1 }
        }
        fn scoped() @suspends -> Int32 { join { 1.branch_task(), 2.branch_task() }; 42 }
        fn failure() @suspends -> Int32 { (1 / delay(0)).to_i32(); record(99) }
    "#;
    let fixture = ScratchDir::new("method_pending");
    let dir = fixture.path();
    let object = dir.join("methods.o");
    let host = dir.join("host.c");
    let program = dir.join("methods");
    let mut compiler = AotCompiler::new("method_pending").unwrap();
    let exports = compiler
        .compile_module(&crate::parser::parse(source).unwrap())
        .unwrap();
    assert!(!exports.iter().any(|name| name.contains('.')));
    let pending = compiler.cps_frame_slots("pending").unwrap();
    let receiver = compiler.cps_frame_slots("receiver").unwrap();
    let scoped = compiler.cps_frame_slots("scoped").unwrap();
    let failure = compiler.cps_frame_slots("failure").unwrap();
    compiler.write_object(&object).unwrap();
    std::fs::write(&host, format!(r#"
        #include "joky_cps.h"
        #include "joky_runtime.h"
        extern int32_t pending(JokyCpsFrame *), receiver(JokyCpsFrame *), scoped(JokyCpsFrame *), failure(JokyCpsFrame *);
        static int ready, count;
        static int events[32];
        int32_t record(int32_t n) {{ events[count++] = n; return n; }}
        int32_t delay(JokyCpsFrame *frame) {{
            if (joky_cps_state(frame) == 0) {{
                record((int32_t)__joky_cps_slots(frame)[0]);
                joky_cps_set_state(frame, 1);
                return JOKY_CPS_PENDING;
            }}
            if (!ready) return JOKY_CPS_PENDING;
            joky_cps_set_result(frame, __joky_cps_slots(frame)[0]);
            return JOKY_CPS_COMPLETED;
        }}
        int main(void) {{
            JokyCpsFrame *a = joky_cps_frame_new({pending}), *b = joky_cps_frame_new({receiver});
            JokyCpsFrame *c = joky_cps_frame_new({scoped}), *d = joky_cps_frame_new({failure});
            JokyCpsFrame *cancelled = joky_cps_frame_new({pending});
            for (int i = 0; i < 3; i++) {{
                if (pending(a) != 1 || receiver(b) != 1 || scoped(c) != 1 || failure(d) != 1 || pending(cancelled) != 1) return 1;
            }}
            int initial[] = {{40,2,2,42,1,2,0,40,2,2}};
            if (count != 10) return 2;
            for (int i = 0; i < 10; i++) if (events[i] != initial[i]) return 3;
            joky_cps_frame_free(cancelled);
            ready = 1;
            if (pending(a) != 0 || joky_cps_result(a) != 141) return 4;
            if (receiver(b) != 1 || receiver(b) != 0 || joky_cps_result(b) != 42) return 5;
            if (scoped(c) != 0 || joky_cps_result(c) != 42) return 6;
            if (failure(d) != JOKY_CPS_RUNTIME_ERROR) return 7;
            if (count != 14 || events[10] != 7 || events[11] != 7 || events[12] != 1 || events[13] != 2) return 8;
            joky_cps_frame_free(a); joky_cps_frame_free(b); joky_cps_frame_free(c); joky_cps_frame_free(d);
            return 0;
        }}
    "#)).unwrap();
    link(
        link_runtime(
            Command::new("cc")
                .arg(&host)
                .arg(&object)
                .args(["-Wall", "-Wextra", "-Werror", "-I"])
                .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("include")),
        ),
        &program,
    );
    let output = Command::new(program).output().unwrap();
    assert!(output.status.success(), "{output:?}");
}

#[test]
#[cfg(unix)]
fn checked_integer_method_matrix_matches_native_direct_and_cps() {
    for suspends in [false, true] {
        let source = include_str!("../../../../tests/fixtures/checked_conversions.jk");
        let source = with_suspends(source, suspends, Suspends::Named("check"));
        let mut compiler = AotCompiler::new("checked_conversions").unwrap();
        compiler
            .compile_module(&crate::parser::parse(&source).unwrap())
            .unwrap();
        assert_eq!(
            compiler.requires_runtime(),
            suspends || cfg!(all(target_os = "macos", target_arch = "aarch64"))
        );
        let fixture = ScratchDir::new("checked_conversions");
        let dir = fixture.path();
        let object = dir.join("checked.o");
        let host = dir.join("host.c");
        let program = dir.join("checked");
        let harness = if suspends {
            let slots = compiler.cps_frame_slots("check").unwrap();
            format!("#include \"joky_cps.h\"\nextern int32_t check(JokyCpsFrame *); int main(void) {{ JokyCpsFrame *f = joky_cps_frame_new({slots}); int status = check(f); int result = (int)joky_cps_result(f); joky_cps_frame_free(f); return status || result; }}")
        } else {
            "#include <stdint.h>\nextern int32_t check(void); int main(void) { return check(); }"
                .into()
        };
        std::fs::write(&host, harness).unwrap();
        compiler.write_object(&object).unwrap();
        let mut command = Command::new("cc");
        command
            .arg(host)
            .arg(object)
            .args(["-Wall", "-Wextra", "-Werror", "-I"])
            .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("include"));
        if suspends || cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            link_runtime(&mut command);
        }
        let output = command.arg("-o").arg(&program).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let output = Command::new(program).output().unwrap();
        assert!(output.status.success(), "suspends={suspends}: {output:?}");
    }
}

#[test]
#[cfg(unix)]
fn borrowed_receivers_execute_in_standalone_native_code_and_pending_frames() {
    let source = format!(
        r#"
        {}
        extern fn pause() @suspends -> Int32
        impl Counter {{
            fn later(&mut self) @suspends -> Int32 {{ self.add(pause()); self.read() }}
        }}
        fn task() @suspends -> Int32 {{ alloc(Counter {{ value: 20 }}).later() }}
    "#,
        include_str!("../../../../tests/fixtures/borrowed_methods.jk")
    );
    let fixture = ScratchDir::new("borrowed-methods");
    let dir = fixture.path();
    let object = dir.join("methods.o");
    let host = dir.join("host.c");
    let program = dir.join("methods");
    let mut compiler = AotCompiler::new("borrowed-methods").unwrap();
    let exports = compiler
        .compile_module(&crate::parser::parse(&source).unwrap())
        .unwrap();
    assert!(!exports.iter().any(|name| name.contains('.')));
    let slots = compiler.cps_frame_slots("task").unwrap();
    compiler.write_object(&object).unwrap();
    std::fs::write(&host, format!(r#"
        #include <stdint.h>
        #include "joky_runtime.h"
        #include "joky_cps.h"
        extern int32_t borrowed_method_locals(void), borrowed_method_fields(void), borrowed_method_control(void);
        extern int32_t borrowed_method_layouts(void), borrowed_method_patterns(void), borrowed_method_shared_content(void);
        extern int32_t task(JokyCpsFrame *);
        static int ready, calls;
        int32_t pause(JokyCpsFrame *frame) {{
            if (!joky_cps_state(frame)) {{ calls++; joky_cps_set_state(frame, 1); }}
            if (!ready) return 1;
            joky_cps_set_result(frame, 22);
            return 0;
        }}
        int main(void) {{
            for (int i = 0; i < 3; i++) {{
                if (borrowed_method_locals() != 42 || borrowed_method_fields() != 42 || borrowed_method_control() != 42 || borrowed_method_layouts() != 42 || borrowed_method_patterns() != 42 || borrowed_method_shared_content() != 42) return 1;
                ready = calls = 0;
                JokyCpsFrame *frame = joky_cps_frame_new({slots});
                if (task(frame) != 1 || task(frame) != 1 || calls != 1) return 2;
                ready = 1;
                if (task(frame) != 0 || joky_cps_result(frame) != 42 || calls != 1) return 3;
                joky_cps_frame_free(frame);
                ready = 0;
                frame = joky_cps_frame_new({slots});
                if (task(frame) != 1) return 4;
                joky_cps_frame_free(frame);
                if (joky_runtime_error_code()) return 5;
            }}
            return 0;
        }}
    "#)).unwrap();
    link(link_runtime(c_compiler().arg(&host).arg(&object)), &program);
    let output = Command::new(program).output().unwrap();
    assert!(output.status.success(), "{output:?}");
}

#[test]
#[cfg(unix)]
fn static_traits_execute_native_calls_and_pending_frames() {
    let source = include_str!("../../../../tests/fixtures/static_traits.jk");
    let fixture = ScratchDir::new("static-traits");
    let dir = fixture.path();
    let object = dir.join("traits.o");
    let host = dir.join("host.c");
    let program = dir.join("traits");
    let mut compiler = AotCompiler::new("static-traits").unwrap();
    let exports = compiler
        .compile_module(&crate::parser::parse(source).unwrap())
        .unwrap();
    assert!(!exports
        .iter()
        .any(|name| name.contains("joky/") || name.contains('.')));
    let slots = compiler.cps_frame_slots("trait_task").unwrap();
    compiler.write_object(&object).unwrap();
    std::fs::write(&host, format!(r#"
        #include <stdint.h>
        #include "joky_runtime.h"
        #include "joky_cps.h"
        extern int32_t trait_locals(void), trait_generics(void), trait_layouts(void), trait_factory(void), trait_dispatch(void);
        extern int32_t trait_task(JokyCpsFrame *);
        static int ready;
        int32_t pause(JokyCpsFrame *frame) {{
            if (!ready) return 1;
            joky_cps_set_result(frame, __joky_cps_slots(frame)[0]);
            return 0;
        }}
        int main(void) {{
            for (int i = 0; i < 3; i++) {{
                if (trait_locals() != 42 || trait_generics() != 42 || trait_layouts() != 42 || trait_factory() != 42 || trait_dispatch() != 42) return 1;
                ready = 0;
                JokyCpsFrame *frame = joky_cps_frame_new({slots});
                if (trait_task(frame) != 1 || trait_task(frame) != 1) return 2;
                ready = 1;
                if (trait_task(frame) != 0 || joky_cps_result(frame) != 42) return 3;
                joky_cps_frame_free(frame);
                ready = 0;
                frame = joky_cps_frame_new({slots});
                if (trait_task(frame) != 1) return 4;
                joky_cps_frame_free(frame);
                if (joky_runtime_error_code()) return 5;
            }}
            return 0;
        }}
    "#)).unwrap();
    link(link_runtime(c_compiler().arg(&host).arg(&object)), &program);
    let output = Command::new(program).output().unwrap();
    assert!(output.status.success(), "{output:?}");
}

#[test]
#[cfg(unix)]
fn trait_defaults_associated_owners_execute_native_lifecycle() {
    let source = include_str!("../../../../tests/fixtures/trait_associated_lifecycle.jk");
    let fixture = ScratchDir::new("associated-defaults");
    let object = fixture.path().join("defaults.o");
    let host = fixture.path().join("host.c");
    let program = fixture.path().join("defaults");
    let mut compiler = AotCompiler::new("associated-defaults").unwrap();
    compiler
        .compile_module(&crate::parser::parse(source).unwrap())
        .unwrap();
    let slots = compiler.cps_frame_slots("default_task").unwrap();
    compiler.write_object(&object).unwrap();
    std::fs::write(&host,format!(r#"
        #include <stdint.h>
        #include "joky_runtime.h"
        #include "joky_cps.h"
        extern int32_t default_discard(void), default_failure(void), default_task(JokyCpsFrame *);
        static int ready, count;
        static int32_t events[16];
        int32_t record(int32_t value) {{ if (count < 16) events[count] = value; ++count; return value; }}
        int32_t pause(JokyCpsFrame *frame) {{
            if (!ready) return JOKY_CPS_PENDING;
            joky_cps_set_result(frame,__joky_cps_slots(frame)[0]); return JOKY_CPS_COMPLETED;
        }}
        int main(void) {{
            for (int run = 0; run < 3; ++run) {{
                count = 0;
                if (default_discard() != 42 || count != 0) return 1;
                count = 0; (void)default_failure();
                if (!joky_runtime_error_code() || count != 0) return 2;
                joky_runtime_error_clear();
                count = 0; ready = 0;
                JokyCpsFrame *frame = joky_cps_frame_new({slots});
                for (int poll = 0; poll < 5; ++poll) {{
                    if (default_task(frame) != JOKY_CPS_PENDING || count != 1 || events[0] != 10) return 3;
                }}
                ready = 1;
                if (default_task(frame) != JOKY_CPS_COMPLETED || joky_cps_result(frame) != 42 || count != 2 || events[1] != 20) return 4;
                joky_cps_frame_free(frame);
                count = 0; ready = 0;
                frame = joky_cps_frame_new({slots});
                if (default_task(frame) != JOKY_CPS_PENDING) return 5;
                joky_cps_frame_free(frame);
                if (count != 1 || events[0] != 10 || joky_runtime_error_code()) return 6;
            }}
            return 0;
        }}
    "#)).unwrap();
    link(link_runtime(c_compiler().arg(host).arg(object)), &program);
    let output = Command::new(program).output().unwrap();
    assert!(output.status.success(), "{output:?}");
}
