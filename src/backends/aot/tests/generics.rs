//! Generics AOT fixtures.
use super::*;

#[test]
#[cfg(unix)]
fn generic_native_objects_preserve_aggregate_specializations_and_pending_frames() {
    for suspends in [false, true] {
        let mut source = with_suspends(
            include_str!("../../../../tests/fixtures/generics.jk"),
            suspends,
            Suspends::Typed,
        );
        source.push_str(r#"
            extern fn record(n: UInt64) -> UInt64
            extern fn delay(n: UInt64) @suspends -> UInt64
            enum Deferred<T> { Empty, Value(T) }
            type Saved<T> = Deferred<T>
            type Value<T> = T
            impl<T> Deferred<T> {
                fn value(value: T) -> Self { Self.Value(value) }
                fn hold<U>(self, keep: U, n: UInt64) @suspends -> (Self, U) { delay(n); (self, keep) }
                fn unwrap(self, fallback: T) -> T { match self { Empty => fallback, Value(value) => value } }
                fn bad<U>(self, keep: U, n: UInt64) -> (Self, U) { 1 << n; (self, keep) }
            }
            fn hold_enum<T>(value: Saved<T>, n: UInt64) @suspends -> Deferred<T> {
                let (saved, extra) = value.hold::<(Bool, UInt64)>((true, 0xffff_ffff_ffff_ffff), n)
                let (valid, wide) = extra
                if !valid || wide != 0xffff_ffff_ffff_ffff { 1 / 0; }
                saved
            }
            fn delayed<T>(value: Value<T>, n: UInt64) @suspends -> Value<T> {
                var saved = value
                let result = Option.None::<Bool>.zip(Option.Some({ saved = hold_enum(Deferred.value(value), n).unwrap(value) saved }))
                saved
            }
            fn keep<T>(a: T, b: T) -> T { a }
            fn bad<T>(value: T, n: UInt64) -> T { let (saved, extra) = Deferred.Value(value).bad(false, n) saved.unwrap(value) }
            fn bad_delayed<T>(value: T, n: UInt64) @suspends -> T {
                let err: Result<Unit, Bool> = Err(false)
                let result = err.and::<T>(Ok(Option.Some(value).unwrap_or(bad(value, delay(n)))))
                value
            }
            fn branch_task<T>(value: T, n: UInt64) @suspends { let _ = delayed(value, n) record(n); }
            fn bad_branch<T>(value: T) @suspends { bad_delayed(value, 64); record(99); }
            fn pending(n: UInt64) @suspends -> UInt64 {
                var pair = Pair { low: -1, high: n }
                let saved = Option.Some(pair).or(Option.Some(delayed({ pair.high = 0 pair }, record(n)))).unwrap_or(pair)
                let new = delayed(saved, 3)
                new.high + uint64(new.low) + pair.high
            }
            fn failure(n: UInt64) @suspends -> UInt64 { bad_delayed((42, true), n); record(99) }
            fn scoped() @suspends -> UInt64 { parallel { | branch_task(Pair { low: 1, high: 2 }, 7) | branch_task::<Unit>({}, 8) } 42 }
            fn failed_scope() @suspends -> UInt64 { parallel { | bad_branch(false) | branch_task({}, 9) } record(99) }
        "#);
        let fixture = ScratchDir::new("generics");
        let dir = fixture.path();
        let object = dir.join("generics.o");
        let program = dir.join("generics");
        let host = dir.join("host.c");
        let mut compiler = AotCompiler::new("generics").unwrap();
        let exports = compiler
            .compile_module(&crate::parser::parse(&source).unwrap())
            .unwrap();
        assert!(!exports
            .iter()
            .any(|name| name == "identity" || name.starts_with("__joky_generic_")));
        assert_eq!(compiler.cps_frame_slots("delayed"), None);
        assert!(compiler.requires_runtime());
        let (declarations, check) = if suspends {
            let slots = compiler.cps_frame_slots("check").unwrap();
            ("extern int32_t check(JokyCpsFrame *);", format!("JokyCpsFrame *c = joky_cps_frame_new({slots}); if (check(c) != 0 || joky_cps_result(c) != 42) return 1; joky_cps_frame_free(c);"))
        } else {
            (
                "extern int32_t check(void);",
                "if (check() != 42) return 1;".to_owned(),
            )
        };
        let mut harness = r#"
            #include "joky_cps.h"
            #include "joky_runtime.h"
            static int ready, events;
            uint64_t record(uint64_t n) { events++; return n; }
            int32_t delay(JokyCpsFrame *f) {
                if (!joky_cps_state(f)) { events++; joky_cps_set_state(f, 1); return 1; }
                if (!ready) return 1;
                joky_cps_set_result(f, __joky_cps_slots(f)[0]); return 0;
            }
            DECLARATIONS
            extern int32_t pending(JokyCpsFrame *), failure(JokyCpsFrame *), scoped(JokyCpsFrame *), failed_scope(JokyCpsFrame *);
            int main(void) {
                CHECK
                JokyCpsFrame *p = joky_cps_frame_new(PENDING);
                JokyCpsFrame *f = joky_cps_frame_new(FAILURE);
                JokyCpsFrame *s = joky_cps_frame_new(SCOPE);
                JokyCpsFrame *bad = joky_cps_frame_new(BADSCOPE);
                JokyCpsFrame *cancelled = joky_cps_frame_new(PENDING);
                uint64_t high = UINT64_C(0x80000000ffffffff);
                __joky_cps_slots(p)[0] = high;
                __joky_cps_slots(f)[0] = 64;
                __joky_cps_slots(cancelled)[0] = 7;
                for (int i = 0; i < 3; i++) {
                    if (pending(p) != 1 || failure(f) != 1 || scoped(s) != 1 || failed_scope(bad) != 1 || pending(cancelled) != 1) return 2;
                }
                if (events != 9) return 3;
                joky_cps_frame_free(cancelled);
                ready = 1;
                if (pending(p) != 1 || pending(p) != 0 || joky_cps_result(p) != high - 1) return 4;
                if (failure(f) != JOKY_CPS_RUNTIME_ERROR || !joky_runtime_error_code() || joky_cps_state(f) != JOKY_CPS_FAILED) return 5;
                joky_runtime_error_clear();
                if (failed_scope(bad) != JOKY_CPS_RUNTIME_ERROR || !joky_runtime_error_code() || joky_cps_state(bad) != JOKY_CPS_FAILED) return 6;
                joky_runtime_error_clear();
                if (scoped(s) != 0 || joky_cps_result(s) != 42 || events != 12) return 7;
                joky_cps_frame_free(p); joky_cps_frame_free(f); joky_cps_frame_free(s); joky_cps_frame_free(bad);
                return 0;
            }
        "#.replace("DECLARATIONS", declarations).replace("CHECK", &check);
        for (marker, name) in [
            ("PENDING", "pending"),
            ("FAILURE", "failure"),
            ("BADSCOPE", "failed_scope"),
            ("SCOPE", "scoped"),
        ] {
            harness = harness.replace(marker, &compiler.cps_frame_slots(name).unwrap().to_string());
        }
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
fn unused_generic_templates_have_no_exports_or_runtime_dependencies() {
    let source = "struct Marker<T> {} impl<T> Marker<T> { fn unused<U>(self, value: U) @suspends -> Self { 1 / 0; self } } fn unused<T>(x: T) @suspends -> T { 1 / 0; x } fn id<T>(x: T) -> T { x } fn answer() -> Int32 { id(42) }";
    let mut compiler = AotCompiler::new("unused_generics").unwrap();
    assert_eq!(
        compiler
            .compile_module(&crate::parser::parse(source).unwrap())
            .unwrap(),
        ["answer"]
    );
    assert!(!compiler.requires_runtime());
}

#[test]
#[cfg(unix)]
fn generic_type_native_objects_preserve_direct_and_cps_specializations() {
    for fixture in [
        include_str!("../../../../tests/fixtures/generic_enums.jk"),
        include_str!("../../../../tests/fixtures/generic_records.jk"),
        include_str!("../../../../tests/fixtures/generic_aliases.jk"),
        include_str!("../../../../tests/fixtures/generic_impls.jk"),
        include_str!("../../../../tests/fixtures/generic_methods.jk"),
        include_str!("../../../../tests/fixtures/generic_library_methods.jk"),
        include_str!("../../../../tests/fixtures/option_result_methods.jk"),
        r#"
            import model.{type Saved as Alias, make, first}
            struct Hidden<T> { wrong: T }
            fn check() -> Int32 { let p: Alias<Int64> = make(int64(42)) int32(first(p)) }
            fn wide() -> UInt64 { let value: UInt64 = 0xffff_ffff_ffff_ffff first(make(value)) }
        "#,
        r#"
            import model.{type Pair as Renamed}
            struct Pair<T> { wrong: T }
            fn helper<T>(value: T) -> Bool { false }
            fn check() -> Int32 { let (value, valid) = Renamed.make(0, 42).swap().zip(true) if valid { value } else { 0 } }
            fn wide() -> UInt64 { let (value, extra) = model.Pair::<UInt64>.zip::<Unit>(model.Pair::<UInt64>.make(0xffff_ffff_ffff_ffff, 0), {}) value }
        "#,
    ] {
        for suspends in [false, true] {
            let source = with_suspends(fixture, suspends, Suspends::Typed);
            let fixture = ScratchDir::new("generic_types");
            let dir = fixture.path();
            let object = dir.join("generic_types.o");
            let program = dir.join("generic_types");
            let host = dir.join("host.c");
            let mut compiler = AotCompiler::new("generic_types").unwrap();
            compiler.set_prelude_enabled(false).unwrap();
            let mut modules = crate::ModuleSources::new();
            modules
                .insert(
                    "model",
                    with_suspends(
                        r#"
                struct Hidden<T> { value: T }
                pub type Saved<T> = Hidden<T>
                pub fn make<T>(value: T) -> Saved<T> { Hidden { value: value } }
                pub fn first<T>(p: Saved<T>) -> T { p.value }
                fn helper<T>(value: T) -> T { value }
                pub struct Pair<T> { left: T, right: T }
                impl<T> Pair<T> {
                    pub fn make(left: T, right: T) -> Self { Self { left: left, right: right } }
                    fn private(self) -> T { self.left }
                    pub fn first(self) -> T { self.private() }
                    pub fn swap(self) -> Self { Self.make(self.right, self.left) }
                    fn private_zip<U>(self, value: U) -> (T, U) { (self.private(), helper(value)) }
                    pub fn zip<U>(self, value: U) -> (T, U) { self.private_zip::<U>(value) }
                }
            "#,
                        suspends,
                        Suspends::Typed,
                    ),
                )
                .unwrap();
            let ast = compiler
                .types
                .resolve_with_modules(&crate::parser::parse(&source).unwrap(), &modules)
                .unwrap();
            compiler.compile_module(&ast).unwrap();
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
}

#[test]
#[cfg(unix)]
fn imported_constructor_aliases_execute_in_direct_and_cps_objects() {
    for suspends in [false, true] {
        let source = with_suspends(
            include_str!("../../../../tests/fixtures/imports.jk"),
            suspends,
            Suspends::Typed,
        );
        let fixture = ScratchDir::new("imports");
        let dir = fixture.path();
        let object = dir.join("imports.o");
        let program = dir.join("imports");
        let host = dir.join("host.c");
        let mut compiler = AotCompiler::new("imports").unwrap();
        compiler.set_prelude_enabled(!suspends).unwrap();
        compiler
            .compile_module(&crate::parser::parse(&source).unwrap())
            .unwrap();
        let harness = if suspends {
            let slots = compiler.cps_frame_slots("check").unwrap();
            format!(
                r#"
                #include "joky_cps.h"
                extern int32_t check(JokyCpsFrame *);
                int main(void) {{
                    JokyCpsFrame *frame = joky_cps_frame_new({slots});
                    int status = check(frame);
                    int failed = status != 0 || joky_cps_result(frame) != 42;
                    joky_cps_frame_free(frame);
                    return failed;
                }}
            "#
            )
        } else {
            "#include <stdint.h>\nextern int32_t check(void);\nint main(void) { return check() != 42; }".into()
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
fn fixed_array_f1_const_parameters_execute_in_standalone_aot() {
    let fixture = ScratchDir::new("const-generics");
    let object = fixture.path().join("const-generics.o");
    let host = fixture.path().join("host.c");
    let program = fixture.path().join("const-generics");
    let mut compiler = AotCompiler::new("const_generics").unwrap();
    let mut modules = crate::ModuleSources::new();
    modules
        .insert(
            "const_model",
            include_str!("../../../../tests/fixtures/const_generics_model.jk"),
        )
        .unwrap();
    let ast = compiler
        .types
        .resolve_with_modules(
            &crate::parser::parse(include_str!("../../../../tests/fixtures/const_generics.jk"))
                .unwrap(),
            &modules,
        )
        .unwrap();
    compiler.compile_module(&ast).unwrap();
    compiler.write_object(&object).unwrap();
    std::fs::write(&host,"#include <stdint.h>\nextern int32_t check(void);\nint main(void) { return check() != 42; }\n").unwrap();
    link(
        link_runtime(
            Command::new("cc")
                .arg(&host)
                .arg(&object)
                .args(["-Wall", "-Wextra", "-Werror"]),
        ),
        &program,
    );
    let output = Command::new(program).output().unwrap();
    assert!(output.status.success(), "{output:?}");
}
