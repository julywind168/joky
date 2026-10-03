#ifndef JOKY_VM_H
#define JOKY_VM_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Same-thread handles, never reused. Zero is invalid. */
typedef uint64_t JokyVmHandle;

/* Borrowed UTF-8 descriptor. No terminator; embedded NUL bytes are preserved.
 * Descriptor and bytes remain live and immutable for the complete call (through
 * completion/cancellation for CPS inputs). Nonempty data must not be NULL.
 * This is not the language String handle or the former length-prefixed ABI. */
typedef struct JokyText {
    const uint8_t *data;
    uint64_t byte_length;
} JokyText;

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
int32_t joky_vm_load_source_v2(JokyVmHandle vm, const JokyText *source);
int32_t joky_vm_reload_source_v2(JokyVmHandle vm, const JokyText *source);
int32_t joky_vm_call_int32_0_v2(JokyVmHandle vm, const JokyText *name);
int32_t joky_vm_call_int32_1_v2(JokyVmHandle vm, const JokyText *name, int32_t a);
int32_t joky_vm_call_int32_2_v2(JokyVmHandle vm, const JokyText *name, int32_t a, int32_t b);

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
