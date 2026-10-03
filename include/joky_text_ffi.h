#ifndef JOKY_TEXT_FFI_H
#define JOKY_TEXT_FFI_H
#include "joky_vm.h"
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

#define JOKY_TEXT_FFI_ABI_V1 1
#define JOKY_TEXT_SPAN 0
#define JOKY_TEXT_C_STRING 1
#define JOKY_TEXT_BUFFER 2
#define JOKY_TEXT_SCALAR 3

/* Initialized to zero before every call. A nonnull release_token transfers one
 * release obligation on BOTH success and failure, even for empty output.
 * It must have a matching descriptor.release. Data is read only on success.
 * byte_length is a span length, or a readable bound including NUL for C strings.
 * For BUFFER, data is ignored and byte_length must not exceed scratch capacity.
 * For SCALAR, scalar holds the low bits of the declared integer result. */
typedef struct JokyTextFfiOutputV1 {
    const uint8_t *data;
    uint64_t byte_length;
    void *release_token;
    uint64_t scalar;
} JokyTextFfiOutputV1;

/* Zero status means success. Nonzero means host failure. Do not retain inputs,
 * scratch or output slots, unwind, or reenter with conflicting accesses.
 * Input lengths exclude any appended NUL. Returned bytes must remain readable
 * after this callback returns until the runtime copies them and calls release.
 * They may alias inputs or scratch; callback-local stack arrays are invalid. */
typedef int32_t (*JokyTextFfiCallV1)(void *context, const JokyText *inputs,
    size_t input_count, uint8_t *scratch, size_t capacity,
    JokyTextFfiOutputV1 *output);
typedef void (*JokyTextFfiReleaseV1)(void *token);

/* The runtime cannot prove pointer validity or prevent arbitrary C overruns.
 * Release must be synchronous, infallible and must not access language values.
 * All conversions and output copies are bounded; temporary buffers consume the
 * task's 64 MiB arena budget and are refunded before returning to language code.
 * AOT imports link this const descriptor as __joky_text_ffi_v1_<prefix><name>.
 * Use AotCompiler::register_text_ffi_fn to select this ABI before compilation. */
typedef struct JokyTextFfiHostV1 {
    uint32_t abi_version;
    uint32_t flags; /* Reserved, must be zero. No asynchronous retention. */
    uint32_t input_count; /* 0..4; remaining kinds and limits must be zero. */
    uint32_t input_kinds[4]; /* SPAN or C_STRING. */
    uint64_t input_limits[4]; /* E.g. INT_MAX for a real C int length. */
    uint32_t output_kind;
    uint64_t output_limit; /* <= 64 MiB; zero for SCALAR. Includes C string NUL. */
    void *context;
    JokyTextFfiCallV1 call;
    JokyTextFfiReleaseV1 release;
} JokyTextFfiHostV1;

#ifdef __cplusplus
}
#endif
#endif
