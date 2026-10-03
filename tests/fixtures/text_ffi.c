#include "joky_text_ffi.h"
#include <limits.h>
#include <stdlib.h>
#include <string.h>

#ifdef TEXT_FFI_VM_TEST
static void (*runtime_failure)(size_t, size_t);
void text_ffi_bind_error(void (*callback)(size_t, size_t)) { runtime_failure = callback; }
#else
#include "joky_runtime.h"
#define runtime_failure __joky_runtime_division_by_zero
#endif
static int calls, frees;
void text_ffi_reset(void) { calls = frees = 0; }
int text_ffi_calls(void) { return calls; }
int text_ffi_frees(void) { return frees; }
static void release(void *token) { ++frees; free(token); }

/* Actual C APIs: a bounded byte writer and an allocating NUL-string copy. */
static int write_text(const uint8_t *data, int length, uint8_t *out, size_t cap) {
    if ((size_t)length > cap) return -1;
    if (length) memcpy(out, data, (size_t)length);
    return length;
}
static char *copy_c_string(const char *text) {
    size_t length = strlen(text) + 1;
    char *result = malloc(length);
    if (result) memcpy(result, text, length);
    return result;
}

static int32_t adapt(void *context, const JokyText *inputs, size_t count,
    uint8_t *scratch, size_t capacity, JokyTextFfiOutputV1 *out) {
    ++calls;
    int mode = (int)(uintptr_t)context;
    if (count) { out->data = inputs[0].data; out->byte_length = inputs[0].byte_length; }
    switch (mode) {
    case 0: return 0; /* Borrow input until runtime has copied it. */
    case 1: {
        char *copy = copy_c_string((const char *)inputs[0].data);
        if (!copy) return 1;
        out->release_token = copy;
        out->data = (const uint8_t *)copy;
        out->byte_length = strlen(copy) + 1;
        return 0;
    }
    case 2: case 3: case 4: case 8: case 9: case 12: case 16: {
        size_t length = mode == 12 ? 64u * 1024u * 1024u : 4;
        uint8_t *copy = calloc(length, 1);
        if (!copy) return 1;
        out->release_token = copy;
        out->data = copy;
        out->byte_length = length;
        if (mode == 16) { out->data = (const uint8_t *)(uintptr_t)1; runtime_failure(1, 2); return 0; }
        if (mode == 2) copy[0] = 0xff;
        if (mode == 3) out->byte_length = UINT64_MAX;
        if (mode == 4) { out->data = (const uint8_t *)(uintptr_t)1; return 17; }
        if (mode == 8) { memcpy(copy, "abcd", 4); }
        if (mode == 9) { out->data = NULL; out->byte_length = 0; }
        return 0;
    }
    case 5: {
        int written = write_text(inputs[0].data, (int)inputs[0].byte_length, scratch, capacity);
        if (written < 0) return 1;
        out->byte_length = (uint64_t)written;
        return 0;
    }
    case 6: out->byte_length = capacity + 1; return 0;
    case 7: out->data = NULL; out->byte_length = 0; return 0;
    case 10: out->scalar = count ? inputs[0].byte_length : 42; return 0;
    case 11: out->byte_length++; return 0; /* Alias temporary NUL input. */
    case 13: out->data = NULL; out->byte_length = 1; return 0;
    case 14:
        if (capacity) scratch[0] = 0xff;
        out->byte_length = 1;
        return 0;
    case 15:
        out->scalar = 0;
        for (size_t i = 0; i < count; ++i) out->scalar += inputs[i].byte_length;
        return 0;
    default: return 1;
    }
}

#define HOST(name, mode, n, input, limit, output, bound) \
    const JokyTextFfiHostV1 __joky_text_ffi_v1_##name = { \
        1, 0, n, {input, 0, 0, 0}, {limit, 0, 0, 0}, output, bound, \
        (void *)(uintptr_t)mode, adapt, release }
HOST(span, 0, 1, JOKY_TEXT_SPAN, INT_MAX, JOKY_TEXT_SPAN, 128);
HOST(cstring, 1, 1, JOKY_TEXT_C_STRING, INT_MAX, JOKY_TEXT_C_STRING, 128);
HOST(invalid, 2, 0, 0, 0, JOKY_TEXT_SPAN, 128);
HOST(overflow, 3, 0, 0, 0, JOKY_TEXT_SPAN, 128);
HOST(failure, 4, 0, 0, 0, JOKY_TEXT_SPAN, 128);
HOST(writer, 5, 1, JOKY_TEXT_SPAN, INT_MAX, JOKY_TEXT_BUFFER, 128);
HOST(overwrite, 6, 0, 0, 0, JOKY_TEXT_BUFFER, 128);
HOST(empty, 7, 0, 0, 0, JOKY_TEXT_SPAN, 0);
HOST(unterminated, 8, 0, 0, 0, JOKY_TEXT_C_STRING, 4);
HOST(owned_empty, 9, 0, 0, 0, JOKY_TEXT_SPAN, 0);
HOST(length, 10, 1, JOKY_TEXT_SPAN, INT_MAX, JOKY_TEXT_SCALAR, 0);
HOST(temporary, 11, 1, JOKY_TEXT_C_STRING, INT_MAX, JOKY_TEXT_C_STRING, 128);
HOST(exhausted, 12, 0, 0, 0, JOKY_TEXT_SPAN, 64u * 1024u * 1024u);
HOST(null_output, 13, 0, 0, 0, JOKY_TEXT_SPAN, 128);
HOST(bad_writer, 14, 0, 0, 0, JOKY_TEXT_BUFFER, 128);
HOST(short_length, 0, 1, JOKY_TEXT_SPAN, 1, JOKY_TEXT_SPAN, 128);
HOST(noarg_scalar, 10, 0, 0, 0, JOKY_TEXT_SCALAR, 0);
HOST(void_result, 10, 0, 0, 0, JOKY_TEXT_SCALAR, 0);
const JokyTextFfiHostV1 __joky_text_ffi_v1_four = {
    1, 0, 4, {0, 1, 0, 1}, {INT_MAX, INT_MAX, INT_MAX, INT_MAX},
    JOKY_TEXT_SCALAR, 0, (void *)(uintptr_t)15, adapt, release
};

HOST(scratch_exhausted, 7, 0, 0, 0, JOKY_TEXT_BUFFER, 64u * 1024u * 1024u);
HOST(channel_failure, 16, 0, 0, 0, JOKY_TEXT_SPAN, 128);

const JokyTextFfiHostV1 *text_ffi_host(size_t index) {
    static const JokyTextFfiHostV1 *const hosts[] = {
        &__joky_text_ffi_v1_span, &__joky_text_ffi_v1_cstring,
        &__joky_text_ffi_v1_invalid, &__joky_text_ffi_v1_overflow,
        &__joky_text_ffi_v1_failure, &__joky_text_ffi_v1_writer,
        &__joky_text_ffi_v1_overwrite, &__joky_text_ffi_v1_empty,
        &__joky_text_ffi_v1_unterminated, &__joky_text_ffi_v1_owned_empty,
        &__joky_text_ffi_v1_length, &__joky_text_ffi_v1_temporary,
        &__joky_text_ffi_v1_exhausted, &__joky_text_ffi_v1_null_output,
        &__joky_text_ffi_v1_bad_writer, &__joky_text_ffi_v1_short_length,
        &__joky_text_ffi_v1_noarg_scalar, &__joky_text_ffi_v1_void_result,
        &__joky_text_ffi_v1_four, &__joky_text_ffi_v1_scratch_exhausted,
        &__joky_text_ffi_v1_channel_failure
    };
    return index < sizeof(hosts) / sizeof(hosts[0]) ? hosts[index] : NULL;
}
