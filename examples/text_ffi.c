/* Link with the object/headers emitted by cargo run --example text_ffi -- DIR.
 * The actual API returns malloc-owned UTF-8; the adapter transfers its release
 * obligation on success. All access is synchronous; no input pointers escape. */
#include "text_ffi_imports.h"
#include "text_ffi_exports.h"
#include "joky_runtime.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static char *foreign_duplicate(const char *text) {
    size_t size = strlen(text) + 1;
    char *result = malloc(size);
    if (result) memcpy(result, text, size);
    return result;
}

static int32_t duplicate(void *context, const JokyText *inputs, size_t count,
    uint8_t *scratch, size_t capacity, JokyTextFfiOutputV1 *output) {
    (void)context; (void)count; (void)scratch; (void)capacity;
    char *result = foreign_duplicate((const char *)inputs[0].data);
    if (!result) return 1;
    output->data = (const uint8_t *)result;
    output->byte_length = strlen(result) + 1;
    output->release_token = result;
    return 0;
}

const JokyTextFfiHostV1 __joky_text_ffi_v1_c_echo = {
    JOKY_TEXT_FFI_ABI_V1, 0, 1, {JOKY_TEXT_C_STRING, 0, 0, 0},
    {127, 0, 0, 0}, JOKY_TEXT_C_STRING, 128, NULL, duplicate, free
};

int main(void) {
    JokyOwnedTextV2 output = {0};
    if (__joky_user_text_v2_run(&output)) {
        joky_runtime_print_error();
        return 1;
    }
    const char *expected = "Hello, 中🙂";
    int ok = output.byte_length == strlen(expected) &&
        !memcmp(output.data, expected, output.byte_length);
    fwrite(output.data, 1, (size_t)output.byte_length, stdout);
    putchar('\n');
    joky_string_result_free_v2(&output);
    return ok ? 0 : 2;
}
