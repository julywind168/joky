/* Link this file with the example's generated object and the same Joky runtime. */
#include "bindings.h"
#include "joky_runtime.h"

static int32_t read_elements(const int32_t *values, size_t length) {
    int32_t total=0;
    for (size_t i=0; i<length; ++i) total+=values[i];
    return total;
}
int32_t joky_example_read(const int32_t (*values)[4], const uint64_t *length) {
    if (*length>4) return -1;
    /* The checked bound also guarantees that the size_t conversion is exact. */
    return read_elements(*values, (size_t)*length);
}
int32_t joky_example_write(int32_t (*values)[4]) {
    (*values)[0]=8;
    return -1;
}
extern int32_t run(void);
int main(void) {
    joky_runtime_error_clear();
    return run()==42 && !joky_runtime_error_code() ? 0 : 1;
}
