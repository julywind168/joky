//! Exercise the private instance trampoline with independently linked native code.
use super::*;

#[cfg(unix)]
#[test]
fn persistent_instances_aot_bridges_use_real_mixed_scalar_abi_and_root_layout() {
    let fixture = ScratchDir::new("persistent-instances");
    let object = fixture.path().join("instances.o");
    let host = fixture.path().join("host.c");
    let executable = fixture.path().join("instances");
    let mut compiler = AotCompiler::with_symbol_prefix("instances", "probe_").unwrap();
    compiler
        .compile_module(
            &crate::parser::parse(include_str!(
                "../../../../tests/fixtures/persistent_instances.jk"
            ))
            .unwrap(),
        )
        .unwrap();
    let ty = &compiler.exports["make"].signature.ret;
    let size = ty.memory_layout().unwrap().size;
    let mut initialization = String::new();
    for (index, (offset, scalar)) in ty.memory_scalars().unwrap().iter().enumerate() {
        let width = scalar.memory_layout().unwrap().size * 8;
        initialization.push_str(&format!("{{ uint{width}_t value = (uint{width}_t)lanes[{index}]; memcpy(root + {offset}, &value, sizeof(value)); }}\n"));
    }
    compiler.write_object(&object).unwrap();
    let mut harness = r#"
#include <stdint.h>
#include <stddef.h>
#include <string.h>
#include "joky_runtime.h"
extern void __joky_instance_entry_probe_make(const uint64_t *, uint64_t *);
extern void __joky_instance_entry_probe_advance(const uint64_t *, uint64_t *);
extern void __joky_instance_entry_probe_total(const uint64_t *, uint64_t *);
extern void __joky_instance_entry_probe_small(const uint64_t *, uint64_t *);
extern void __joky_instance_entry_probe_fail(const uint64_t *, uint64_t *);
extern void __joky_instance_entry_probe_empty(const uint64_t *, uint64_t *);
extern void __joky_instance_entry_probe_empty_value(const uint64_t *, uint64_t *);
static uint64_t bits(double value) { uint64_t result; memcpy(&result, &value, 8); return result; }
int main(void) {
    union { max_align_t alignment; unsigned char root[ROOT_SIZE]; } storage;
    unsigned char *root = storage.root;
    uint64_t input[] = {10, bits(0.5)}, lanes[4] = {0}, result = 0;
    __joky_instance_entry_probe_make(input, lanes);
    if (joky_runtime_error_code()) return 1;
    INITIALIZE
    uint64_t args[] = {(uint64_t)(uintptr_t)root, 1, bits(0.25)};
    for (int i = 1; i <= 100; ++i) {
        __joky_instance_entry_probe_advance(args, &result);
        if ((int32_t)result != 10 + i || joky_runtime_error_code()) return 2;
    }
    __joky_instance_entry_probe_total(args, &result);
    if (result != bits(25.5)) return 3;
    __joky_instance_entry_probe_small(args, &result);
    if ((int16_t)result != -7) return 4;
    args[1] = 0;
    __joky_instance_entry_probe_fail(args, &result);
    if (!joky_runtime_error_code()) return 5;
    joky_runtime_error_clear();
    __joky_instance_entry_probe_empty(input, lanes);
    args[2] = UINT64_C(0x80000000);
    __joky_instance_entry_probe_empty_value(args, &result);
    if (result != UINT64_C(0x80000000) || joky_runtime_error_code()) return 6;
    return 0;
}
"#
    .replace("ROOT_SIZE", &size.to_string());
    harness = harness.replace("INITIALIZE", &initialization);
    std::fs::write(&host, harness).unwrap();
    link(
        link_runtime(c_compiler().arg(&host).arg(&object)),
        &executable,
    );
    let status = Command::new(executable).status().unwrap();
    assert!(status.success(), "instance bridges: {status}");
}
