#include <stdint.h>
#include <stdio.h>
#include "joky_runtime.h"
static int32_t events[32];
static int count;
void record_drop(int32_t value) { if (count < 32) events[count++] = value; }
extern int32_t ordered_copy(void), ordered_owned(void), ordered_enum_tuple(void);
extern int32_t ordered_wrapper(void), ordered_zero_and_aggregate(void), ordered_references(void);
extern int32_t ordered_drop_failure(void), ordered_invalid_failure(void), ordered_rhs_failure(void);
extern int32_t ordered_evaluation(void);
int main(void) {
    struct Case { int32_t (*call)(void); int failure, count; int32_t events[12]; } cases[] = {
        {ordered_copy, 0, 0, {0}},
        {ordered_evaluation, 0, 3, {10, 20, 30}},
        {ordered_owned, 0, 4, {-1, -2, -3, -4}},
        {ordered_enum_tuple, 0, 2, {-1, -2}},
        {ordered_wrapper, 0, 0, {0}},
        {ordered_zero_and_aggregate, 0, 0, {0}},
        {ordered_references, 0, 0, {0}},

        {ordered_rhs_failure, 1, 2, {-1, -2}},
    };
    for (unsigned i = 0; i < sizeof(cases) / sizeof(cases[0]); i++) {
        count = 0;
        int32_t result = cases[i].call();
        if (cases[i].failure != !!joky_runtime_error_code() || (!cases[i].failure && result != 42)) {
            fprintf(stderr, "Ordered array case %u: result %d, error %d\n", i, result, joky_runtime_error_code());
            return 1;
        }
        if (count != cases[i].count) return 2;
        for (int j = 0; j < count; j++) if (events[j] != cases[i].events[j]) return 3;
        joky_runtime_error_clear();
    }
    return 0;
}
