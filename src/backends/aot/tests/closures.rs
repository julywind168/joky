//! Closure ownership and hidden entries link without compiler-process addresses.
use super::*;

#[test]
#[cfg(unix)]
fn closure_owning_cleanup_executes_in_standalone_native_code() {
    let fixture = ScratchDir::new("closure-cleanup");
    let object = fixture.path().join("closures.o");
    let host = fixture.path().join("host.c");
    let executable = fixture.path().join("closures");
    let mut compiler = AotCompiler::new("closure-cleanup").unwrap();
    compiler
        .compile_module(
            &crate::parser::parse(include_str!("../../../../tests/fixtures/closures.jk")).unwrap(),
        )
        .unwrap_or_else(|e| panic!("{e}"));
    compiler.write_object(&object).unwrap();
    std::fs::write(
        &host,
        r#"
        #include <stdint.h>
        #include "joky_runtime.h"
        static int32_t events[64]; static int count;
        void closure_event(int32_t value) { if (count < 64) events[count++] = value; }
        extern int32_t unused(void), consume(void), repeated(void), skipped(void);
        extern int32_t consumed_helper(void), failure(void), cancellation_work(void), consume_last(void), consuming_failure(void);
        extern int32_t cleanup_failure(void), failure_before_cleanup_failure(void);
        extern int32_t argument_order(void), consuming_argument_failure(void);
        int main(void) {
            struct Case { int32_t (*call)(void); int fails, count; int32_t events[6]; } cases[] = {
                {unused, 0, 4, {-1,-2,1,2}}, {consume, 0, 2, {-1,1}},
                {repeated, 0, 2, {-1,1}}, {skipped, 0, 2, {-1,1}},
                {consumed_helper, 0, 4, {-1,1,-2,2}}, {failure, 1, 4, {-1,-2,1,2}},
                {cancellation_work, 0, 4, {-1,-2,2,1}}, {consume_last, 0, 6, {-1,-2,-3,3,1,2}}, {consuming_failure, 1, 6, {-1,-2,-3,1,2,3}},
                {cleanup_failure, 1, 6, {-4,-5,-1,4,5,1}}, {failure_before_cleanup_failure, 1, 6, {-4,-5,-1,4,5,1}},
                {argument_order, 0, 4, {100,200,300,400}}, {consuming_argument_failure, 1, 2, {-1,1}}
            };
            for (unsigned run = 0; run < 10; ++run) {
                for (unsigned i = 0; i < sizeof(cases)/sizeof(cases[0]); ++i) {
                    count = 0; joky_runtime_error_clear();
                    int32_t result = cases[i].call();
                    if (cases[i].fails != !!joky_runtime_error_code()) return 1;
                    if (!cases[i].fails && result != 42) return 2;
                    if (count != cases[i].count) return 3;
                    for (int j = 0; j < count; ++j) if (events[j] != cases[i].events[j]) return 4;
                }
            }
            return 0;
        }
    "#,
    )
    .unwrap();
    link(
        link_runtime(c_compiler().arg(host).arg(object)),
        &executable,
    );
    assert!(Command::new(executable).status().unwrap().success());
}
