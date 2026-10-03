#ifndef JOKY_VM_H
#define JOKY_VM_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Same-thread handles, never reused. Zero is invalid. */
typedef uint64_t JokyVmHandle;

/* Immutable borrowed String: native-endian byte length followed by UTF-8.
 * No terminator is required; embedded NUL bytes are preserved. The caller owns
 * this allocation and keeps it alive for the complete call. The VM copies or
 * parses the contents synchronously and never retains this pointer. */
typedef struct JokyString {
    uint64_t byte_length;
    unsigned char bytes[];
} JokyString;

/* v2 root results own a host copy, preserve NUL, and survive task/VM cleanup.
 * Initialize output to zero. Free each successful result exactly once below. */
typedef struct JokyOwnedTextV2 {
    uint8_t *data;
    uint64_t byte_length;
} JokyOwnedTextV2;
void joky_string_result_free_v2(JokyOwnedTextV2 *output);

JokyVmHandle joky_vm_create(void);
JokyVmHandle joky_vm_create_interpreter(void);
int32_t joky_vm_close(JokyVmHandle vm);
int32_t joky_vm_load_source(JokyVmHandle vm, const JokyString *source);
int32_t joky_vm_reload_source(JokyVmHandle vm, const JokyString *source);
int32_t joky_vm_call_int32_0(JokyVmHandle vm, const JokyString *name);
int32_t joky_vm_call_int32_1(JokyVmHandle vm, const JokyString *name, int32_t a);
int32_t joky_vm_call_int32_2(JokyVmHandle vm, const JokyString *name, int32_t a, int32_t b);

/* Each operation clears the thread's previous error before starting.
 * create returns zero on failure; close/load/reload return 0 on success, 1 on
 * failure. Calls return 0 on failure: inspect error_code to distinguish it from
 * a successful zero result. These two queries do not clear the error. */
int32_t joky_vm_error_code(void);
void joky_vm_print_error(void);

#ifdef __cplusplus
}
#endif

#endif
