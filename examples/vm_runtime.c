/* Link with the same runtime archive used by joky build. */
#include "joky_vm.h"
#include <string.h>

static JokyText text(const char *value) {
    JokyText result = {(const uint8_t *)value, strlen(value)};
    return result;
}

int main(void) {
    JokyVmHandle vm = joky_vm_create();
    JokyText source = text("fn add(x: Int32, y: Int32) -> Int32 { x + y }");
    int32_t status = joky_vm_load_source_v2(vm, &source);
    if (status != 0) return 1;
    JokyText name = text("add");
    int32_t result = joky_vm_call_int32_2_v2(vm, &name, 20, 22);
    if (result != 42 || joky_vm_error_code() != 0) return 2;
    if (joky_vm_close(vm) != 0) return 3;
    if (joky_vm_close(vm) != 1 || joky_vm_error_code() != 1) return 4;
    return 0;
}
