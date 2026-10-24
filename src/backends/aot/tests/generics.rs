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
            fn delayed<T: Copy>(value: Value<T>, n: UInt64) @suspends -> Value<T> {
                var saved = value
                let result = Option.None::<Bool>.zip(Option.Some({ saved = hold_enum(Deferred.value(value), n).unwrap(value); saved }))
                saved
            }
            fn keep<T>(a: T, b: T) -> T { a }
            fn bad<T: Copy>(value: T, n: UInt64) -> T { let (saved, extra) = Deferred.Value(value).bad(false, n); saved.unwrap(value) }
            fn bad_delayed<T: Copy>(value: T, n: UInt64) @suspends -> T {
                let err: Result<Unit, Bool> = Err(false)
                let result = err.and::<T>(Ok(Option.Some(value).unwrap_or(bad(value, delay(n)))))
                value
            }
            fn branch_task<T: Copy>(value: T, n: UInt64) @suspends { let _ = delayed(value, n); record(n); }
            fn bad_branch<T: Copy>(value: T) @suspends { bad_delayed(value, 64); record(99); }
            fn pending(n: UInt64) @suspends -> UInt64 {
                var pair = Pair { low: -1, high: n }
                let saved = Option.Some(pair).or(Option.Some(delayed({ pair.high = 0; pair }, record(n)))).unwrap_or(pair)
                let new = delayed(saved, 3)
                new.high + uint64(new.low) + pair.high
            }
            fn failure(n: UInt64) @suspends -> UInt64 { bad_delayed((42, true), n); record(99) }
            fn scoped() @suspends -> UInt64 { join { | branch_task(Pair { low: 1, high: 2 }, 7) | branch_task::<Unit>({}, 8) }; 42 }
            fn failed_scope() @suspends -> UInt64 { join { | bad_branch(false) | branch_task({}, 9) }; record(99) }
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
            fn check() -> Int32 { let p: Alias<Int64> = make(int64(42)); int32(first(p)) }
            fn wide() -> UInt64 { let value: UInt64 = 0xffff_ffff_ffff_ffff; first(make(value)) }
        "#,
        r#"
            import model.{type Pair as Renamed}
            struct Pair<T> { wrong: T }
            fn helper<T>(value: T) -> Bool { false }
            fn check() -> Int32 { let (value, valid) = Renamed.make(0, 42).swap().zip(true); if valid { value } else { 0 } }
            fn wide() -> UInt64 { let (value, extra) = model.Pair::<UInt64>.zip::<Unit>(model.Pair::<UInt64>.make(0xffff_ffff_ffff_ffff, 0), {}); value }
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
                impl<T: Copy> Pair<T> {
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

#[test]
#[cfg(unix)]
fn fixed_array_f2a_executes_in_standalone_aot() {
    let fixture = ScratchDir::new("fixed-array-f2a");
    let object = fixture.path().join("arrays.o");
    let host = fixture.path().join("host.c");
    let program = fixture.path().join("arrays");
    let mut compiler = AotCompiler::new("fixed_arrays").unwrap();
    let ast = compiler
        .types
        .resolve_aliases(
            &crate::parser::parse(include_str!(
                "../../../../tests/fixtures/fixed_array_f2a.jk"
            ))
            .unwrap(),
        )
        .unwrap();
    compiler.compile_module(&ast).unwrap();
    compiler.write_object(&object).unwrap();
    std::fs::write(&host, r#"
        #include <stdint.h>
        extern int32_t copies(void), callbacks(void), zero(void), boundary(void), aggregates(void), reuse(void), single(void), whole_reference(void);
        int main(void) {
            return copies() != 42 || callbacks() != 42 || zero() != 42 || boundary() != 42 || aggregates() != 42 || reuse() != 42 || single() != 42 || whole_reference() != 42;
        }
    "#).unwrap();
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

#[test]
#[cfg(unix)]
fn fixed_array_f2a_aot_cleans_callbacks_on_failure_and_empty_input() {
    let fixture = ScratchDir::new("fixed-array-f2a-cleanup");
    let object = fixture.path().join("arrays.o");
    let host = fixture.path().join("host.c");
    let program = fixture.path().join("arrays");
    let mut compiler = AotCompiler::new("fixed_array_cleanup").unwrap();
    compiler
        .compile_module(
            &crate::parser::parse(include_str!(
                "../../../../tests/fixtures/fixed_array_f2a_cleanup.jk"
            ))
            .unwrap(),
        )
        .unwrap();
    compiler.write_object(&object).unwrap();
    std::fs::write(&host, r#"
        #include <stdint.h>
        extern int32_t construct(void), zero_env(void), fail_init(void), early(void), assignment_fail(void), fail_cleanup(void);
        extern int32_t joky_runtime_error_code(void);
        extern void joky_runtime_error_clear(void);
        static int32_t events[16]; static unsigned count;
        void note(int32_t value) { if (count < 16) events[count++] = value; }
        int main(void) {
            if (construct() != 42 || count != 5 || events[0] != 1 || events[3] != 4 || events[4] != -1) return 1;
            count = 0;
            if (zero_env() != 42 || count != 1 || events[0] != -1) return 2;
            count = 0; fail_init();
            if (!joky_runtime_error_code() || count != 4 || events[0] != 1 || events[2] != 3 || events[3] != -1) return 3;
            joky_runtime_error_clear(); count = 0;
            if (early() != 42 || count != 1 || events[0] != -1) return 4;
            count = 0; assignment_fail();
            if (!joky_runtime_error_code() || count != 3 || events[0] != 1 || events[2] != 3) return 5;
            joky_runtime_error_clear(); count = 0; fail_cleanup();
            if (!joky_runtime_error_code() || count != 2 || events[0] != 1 || events[1] != -2) return 6;
            joky_runtime_error_clear(); return 0;
        }
    "#).unwrap();
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

#[cfg(unix)]
fn run_fixed_array_aot(name: &str, source: &str, harness: &str) {
    let fixture = ScratchDir::new(name);
    let object = fixture.path().join("arrays.o");
    let host = fixture.path().join("host.c");
    let program = fixture.path().join("arrays");
    let mut compiler = AotCompiler::new(name).unwrap();
    compiler
        .compile_module(&crate::parser::parse(source).unwrap())
        .unwrap();
    compiler.write_object(&object).unwrap();
    std::fs::write(&host, harness).unwrap();
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

#[test]
#[cfg(unix)]
fn fixed_array_f2b_executes_in_standalone_aot() {
    run_fixed_array_aot(
        "fixed_array_f2b",
        include_str!("../../../../tests/fixtures/fixed_array_f2b.jk"),
        r#"
        #include <stdint.h>
        extern int32_t nested(void), records(void), tuples(void), variants(void), indirect(void), dynamic(void), generated(void), deeper(void), empty_nested(void), pattern(void), borrowed(void), reuse(void), aggregate_indirect(void), aggregate_replace(void), generated_variants(void);
        int main(void) {
            return nested()!=42 || records()!=42 || tuples()!=42 || variants()!=42 || indirect()!=42 || dynamic()!=42 || generated()!=42 || deeper()!=42 || empty_nested()!=42 || pattern()!=42 || borrowed()!=42 || reuse()!=42 || aggregate_indirect()!=42 || aggregate_replace()!=42 || generated_variants()!=42;
        }
    "#,
    );
}

#[test]
#[cfg(unix)]
fn fixed_array_f2b_aot_cleans_nested_initialization_failure() {
    run_fixed_array_aot(
        "fixed_array_f2b_cleanup",
        include_str!("../../../../tests/fixtures/fixed_array_f2b_cleanup.jk"),
        r#"
        #include <stdint.h>
        extern int32_t exercise(void), fail_init(void);
        extern int32_t joky_runtime_error_code(void);
        extern void joky_runtime_error_clear(void);
        static int32_t events[16]; static unsigned count;
        void note(int32_t value) { if (count<16) events[count++]=value; }
        int main(void) {
            if (exercise()!=42 || count!=5 || events[0]!=1 || events[3]!=4 || events[4]!=-1) return 1;
            count=0; fail_init();
            if (!joky_runtime_error_code() || count!=4 || events[0]!=1 || events[2]!=3 || events[3]!=-1) return 2;
            joky_runtime_error_clear(); return 0;
        }
    "#,
    );
}

#[test]
#[cfg(unix)]
fn fixed_array_f3_views_execute_in_standalone_aot() {
    run_fixed_array_aot(
        "fixed_array_f3",
        include_str!("../../../../tests/fixtures/fixed_array_f3.jk"),
        r#"
        #include <stdint.h>
        extern int32_t elements(void), slice_values(void), nested(void), dynamic(void), empty(void), returned(void), indirect(void), generic(void), shared_views(void), element_views(void), aligned(void), noescape(void), fields(void), alias_methods(void), record_method(void), enum_view(void), slice_operations(void);
        extern int32_t bad_index(void), bad_mut_index(void), bad_range(void), bad_end(void), bad_empty(void), bad_zero_size(void);
        extern int32_t joky_runtime_error_code(void);
        extern void joky_runtime_error_clear(void);
        int main(void) {
            int32_t (*valid[])(void)={elements,slice_values,nested,dynamic,empty,returned,indirect,generic,shared_views,element_views,aligned,noescape,fields,alias_methods,record_method,enum_view,slice_operations};
            int32_t (*invalid[])(void)={bad_index,bad_mut_index,bad_range,bad_end,bad_empty,bad_zero_size};
            for(unsigned i=0;i<sizeof(valid)/sizeof(*valid);++i)
                if(valid[i]()!=42 || joky_runtime_error_code()) return 1;
            for(unsigned i=0;i<sizeof(invalid)/sizeof(*invalid);++i) {
                invalid[i](); if(!joky_runtime_error_code()) return 2;
                joky_runtime_error_clear();
            }
            return elements()!=42;
        }
        "#,
    );
}

#[test]
#[cfg(unix)]
fn fixed_array_f3_aot_cleans_views_after_success_and_failure() {
    run_fixed_array_aot(
        "fixed_array_f3_cleanup",
        include_str!("../../../../tests/fixtures/fixed_array_f3_cleanup.jk"),
        r#"
        #include <stdint.h>
        extern int32_t exercise(void), failure(void), payload_reuse(void), headers(void), references(void);
        extern int32_t joky_runtime_error_code(void);
        extern void joky_runtime_error_clear(void);
        static int32_t events[16]; static unsigned count;
        void note(int32_t value) { if(count<16) events[count++]=value; }
        int main(void) {
            if(exercise()!=42 || count!=5 || events[0]!=1 || events[3]!=4 || events[4]!=-1) return 1;
            count=0; failure();
            if(!joky_runtime_error_code() || count!=2 || events[0]!=1 || events[1]!=-1) return 2;
            joky_runtime_error_clear();
            return payload_reuse()!=42 || headers()!=42 || references()!=42 || joky_runtime_error_code();
        }
        "#,
    );
}
