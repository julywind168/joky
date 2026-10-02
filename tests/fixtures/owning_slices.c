#include <stdint.h>
#include <stdio.h>
#include "joky_runtime.h"
static int32_t events[32];
static int count;
void record_drop(int32_t value) { if (count < 32) events[count++] = value; }
extern int32_t slice_shared(void), slice_mutable(void), slice_wrapper(void);
extern int32_t slice_enum(void), slice_tuple(void), slice_empty(void);
extern int32_t slice_bounds(void), slice_sub_bounds(void), slice_drop_failure(void);
int main(void) {
    struct Case { int32_t (*call)(void); int failure, count; int32_t events[12]; } cases[] = {
        {slice_shared, 0, 8, {-1, -2, -3, -4, 4, 3, 2, 1}},
        {slice_mutable, 0, 10, {-1, -2, -3, -4, -5, 2, 5, 13, 4, 1}},
        {slice_wrapper, 0, 0, {0}},
        {slice_enum, 0, 2, {-1, 42}},
        {slice_tuple, 0, 8, {-1, -2, -3, -4, 2, 1, 3, 4}},
        {slice_empty, 0, 2, {-1, 1}},
        {slice_bounds, 1, 2, {-1, 1}},
        {slice_sub_bounds, 1, 2, {-1, 1}},
        {slice_drop_failure, 1, 3, {1, 2, 3}},
    };
    for (unsigned i = 0; i < sizeof(cases) / sizeof(cases[0]); i++) {
        count = 0;
        int32_t result = cases[i].call();
        if (cases[i].failure != !!joky_runtime_error_code() || (!cases[i].failure && result != 42)) {
            fprintf(stderr, "Owning slice case %u: result %d, error %d\n", i, result, joky_runtime_error_code());
            return 1;
        }
        if (count != cases[i].count) return 2;
        for (int j = 0; j < count; j++) if (events[j] != cases[i].events[j]) return 3;
        joky_runtime_error_clear();
    }
    return 0;
}
