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
 * Error offsets refer to the failing module's source, including after reload.
 * Queries do not clear the error. Host callbacks must return promptly after a
 * generated callback fails; do not clear an unhandled failure or unwind.
 */
void joky_runtime_error_clear(void);
int32_t joky_runtime_error_code(void);
size_t joky_runtime_error_start(void);
size_t joky_runtime_error_end(void);
void joky_runtime_print_error(void);

/* Compiler/runtime interface. Records the first failure in this invocation. */
void __joky_runtime_division_by_zero(size_t start, size_t end);

#ifdef __cplusplus
}
#endif
#endif
