/* Link with the same runtime archive used by joky build. */
#include "joky_vm.h"
#include <stdlib.h>
#include <string.h>

static JokyString *text(const char *value) {
    size_t length = strlen(value);
    JokyString *result = malloc(sizeof(*result) + length);
    if (!result) abort();
    result->byte_length = length;
    memcpy(result->bytes, value, length);
    return result;
}

int main(void) {
    JokyVmHandle vm = joky_vm_create();
    JokyString *source = text("fn add(x: Int32, y: Int32) -> Int32 { x + y }");
    int32_t status = joky_vm_load_source(vm, source);
    free(source); /* load_source retained no pointer into this allocation. */
    if (status != 0) return 1;
    JokyString *name = text("add");
    int32_t result = joky_vm_call_int32_2(vm, name, 20, 22);
    free(name);
    if (result != 42 || joky_vm_error_code() != 0) return 2;
    if (joky_vm_close(vm) != 0) return 3;
    if (joky_vm_close(vm) != 1 || joky_vm_error_code() != 1) return 4;
    return 0;
}
