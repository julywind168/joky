#include <stdint.h>
#include <stdio.h>
#include "joky_runtime.h"
static int32_t events[32];
static int count;
void record_drop(int32_t value) { if (count < 32) events[count++] = value; }
extern int32_t remove_copy(void), remove_owned(void), remove_enum_tuple(void);
extern int32_t remove_wrapper(void), remove_zero_and_aggregate(void);
extern int32_t remove_references(void), remove_drop_failure(void);
int main(void) {
    struct Case { int32_t (*call)(void); int failure, count; int32_t events[12]; } cases[] = {
        {remove_copy, 0, 0, {0}},
        {remove_owned, 0, 6, {-1, -2, -3, 1, 2, 3}},
        {remove_enum_tuple, 0, 4, {-1, -2, 2, 1}},
        {remove_wrapper, 0, 0, {0}},
        {remove_zero_and_aggregate, 0, 0, {0}},
        {remove_references, 0, 0, {0}},
        {remove_drop_failure, 1, 3, {1, 2, 3}},
    };
    for (unsigned i = 0; i < sizeof(cases) / sizeof(cases[0]); i++) {
        count = 0;
        int32_t result = cases[i].call();
        if (cases[i].failure != !!joky_runtime_error_code() || (!cases[i].failure && result != 42)) {
            fprintf(stderr, "Swap remove case %u: result %d, error %d\n", i, result, joky_runtime_error_code());
            return 1;
        }
        if (count != cases[i].count) return 2;
        for (int j = 0; j < count; j++) if (events[j] != cases[i].events[j]) return 3;
        joky_runtime_error_clear();
    }
    return 0;
}
