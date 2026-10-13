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
/* Requires the Script handle's active executor. Returns rebound count or -1.
 * Only domains owned by modules loaded through this handle are selected. */
int32_t joky_vm_publish_domain_code(JokyVmHandle vm);
/* Migration requires exactly one live domain owned by this Script on the active
 * executor. Schema is a nonzero, process-local identity; zero indicates error.
 * Preparation generates PreviousState from the actual finite owning schema. A handle
 * owns at most one candidate; publish consumes it on success or validation failure.
 * prepare/publish/cancel return 0 on success, 1 on error. Close drops a candidate. */
uint64_t joky_vm_domain_schema(JokyVmHandle vm);
int32_t joky_vm_prepare_migration_v1(JokyVmHandle vm, uint64_t schema,
    const JokyText *source, const JokyText *method);
/* Incremental start constructs a candidate; step returns 0 ready, 1 pending,
 * -1 error. Positive budget is a cooperative work hint. Once candidate validation
 * starts, step errors and premature publish discard it; earlier context errors
 * may leave it pending. No old-state borrow survives a call. */
int32_t joky_vm_begin_migration_v1(JokyVmHandle vm, uint64_t schema,
    const JokyText *source, const JokyText *start, const JokyText *advance);
int32_t joky_vm_step_migration(JokyVmHandle vm, uint64_t budget);
/* Batch begin captures every live domain owned by this Script (1..256).
 * Stage constructs all instances of the start function's nominal return type.
 * Empty advance selects synchronous construction. Compatible roots stay in place
 * as validated read dependencies. Once candidate validation starts, stage/step
 * errors discard the batch; earlier context/text errors may leave it pending.
 * All types must be staged and all steps done before the shared publish call.
 * begin/stage return 0 success, 1 error. One single or batch plan per handle. */
int32_t joky_vm_begin_migration_batch_v1(JokyVmHandle vm, const JokyText *source);
int32_t joky_vm_stage_migration_v1(JokyVmHandle vm, const JokyText *start,
    const JokyText *advance);
int32_t joky_vm_publish_migration(JokyVmHandle vm);
int32_t joky_vm_cancel_migration(JokyVmHandle vm);
int32_t joky_vm_call_int32_0_v2(JokyVmHandle vm, const JokyText *name);
int32_t joky_vm_call_int32_1_v2(JokyVmHandle vm, const JokyText *name, int32_t a);
int32_t joky_vm_call_int32_2_v2(JokyVmHandle vm, const JokyText *name, int32_t a, int32_t b);
/* Float64 calls return 0.0 on failure: inspect joky_vm_error_code to
 * distinguish it from a successful zero result. */
double joky_vm_call_float64_0_v2(JokyVmHandle vm, const JokyText *name);
double joky_vm_call_float64_1_v2(JokyVmHandle vm, const JokyText *name, double a);
double joky_vm_call_float64_2_v2(JokyVmHandle vm, const JokyText *name, double a, double b);

/* Each synchronous operation clears the thread's previous error before starting.
 * create returns zero on failure; close/load/reload return 0 on success, 1 on
 * failure. Calls return 0 on failure: inspect error_code to distinguish it from
 * a successful zero result. These two queries do not clear the error. */
int32_t joky_vm_error_code(void);
void joky_vm_print_error(void);

#ifdef __cplusplus
}
#endif

#endif
