#ifndef JOKY_RUNTIME_H
#define JOKY_RUNTIME_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Native error channel, local to the calling thread and separate from joky/vm.
 * Link generated code and these functions to the SAME runtime instance.
 * Before each top-level direct call or CPS poll, clear; afterwards check code.
 * A failed direct result is unspecified: never use it, especially String views.
 * A failing CPS poll returns JOKY_CPS_RUNTIME_ERROR and must not be polled again.
 * macOS AArch64 synchronous code also requires this runtime for native stack
 * admission, including pure scalar functions. Admission protects generated
 * bodies; hosts must supply stack space for the ABI entry and bounded runtime
 * callbacks. It cannot recover a host stack overflow or protect arbitrary host
 * recursion. CPS/worker cleanup coverage is a separate contract.
 * Error offsets refer to the failing module's source, including after reload.
 * Native stack exhaustion records fixed-size state. Generated propagation and
 * code/start/end/clear do not allocate its diagnostic; only the nearest caller
 * is retained, with a count of further omitted call sites.
 * print_error materializes the diagnostic and may allocate. Call it after
 * generated code has returned and enough host stack is available.
 * Queries do not clear the error. Host callbacks must return promptly after a
 * generated callback fails; do not clear an unhandled failure or unwind.
 */
void joky_runtime_error_clear(void);
int32_t joky_runtime_error_code(void);
size_t joky_runtime_error_start(void);
size_t joky_runtime_error_end(void);
void joky_runtime_print_error(void);

/* Generated standalone startup only: capture application arguments (without
 * argv[0]), environment and caller directory before entering the bundle root.
 * argc is nonnegative; argv points to argc live NUL-terminated strings.
 * Call once. Returns 0 or -1 with a runtime error. Embedded VMs do not inherit
 * this snapshot; Rust hosts attach ProcessContext explicitly to FileService. */
int32_t joky_process_init_v1(int32_t argc, const char *const *argv);

/* Compiler/runtime interface. Records the first failure in this invocation. */
void __joky_runtime_division_by_zero(size_t start, size_t end);
void __joky_runtime_invalid_shift(size_t start, size_t end);

#ifdef __cplusplus
}
#endif
#endif
