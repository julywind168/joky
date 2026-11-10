#include <stdint.h>
#include <stdio.h>
#include "joky_runtime.h"
static int32_t events[32];
static unsigned count;
void record_drop(int32_t value) { if (count < 32) events[count++] = value; }
extern int32_t mem_copy(void), mem_places(void), mem_layout(void), mem_owners(void);
extern int32_t mem_wrappers(void), mem_enums(void), mem_text(void), mem_references(void);
extern int32_t mem_drop_shared(void), mem_assignment_failure(void), mem_transition(void);
extern int32_t mem_function_values(void), mem_closure(void), mem_fields(void), mem_assignment_drop_failure(void), mem_copy_fields(void), mem_drop_exclusive(void);
int main(void) {
    struct Case { int32_t (*call)(void); int failure; unsigned count; int32_t events[8]; } cases[] = {
        {mem_copy, 0, 0, {0}}, {mem_copy_fields, 0, 0, {0}}, {mem_drop_exclusive, 0, 0, {0}}, {mem_places, 0, 0, {0}}, {mem_layout, 0, 0, {0}},
        {mem_owners, 0, 3, {-2, -2, -4}},
        {mem_wrappers, 0, 0, {0}}, {mem_enums, 0, 0, {0}},
        {mem_text, 0, 0, {0}}, {mem_references, 0, 0, {0}},
        {mem_drop_shared, 0, 1, {-1}}, {mem_assignment_failure, 1, 0, {0}},
        {mem_transition, 0, 3, {-1, -2, -3}}, {mem_function_values, 0, 0, {0}},
        {mem_closure, 0, 1, {-1}}, {mem_fields, 0, 0, {0}},
    };
    for (unsigned i = 0; i < sizeof(cases) / sizeof(cases[0]); ++i) {
        count = 0;
        int32_t result = cases[i].call();
        if (cases[i].failure != !!joky_runtime_error_code() || (!cases[i].failure && result != 42) || count != cases[i].count) {
            fprintf(stderr, "mem case %u: result %d, drops %u\n", i, result, count);
            return 1;
        }
        for (unsigned j = 0; j < count; ++j) if (events[j] != cases[i].events[j]) {
            fprintf(stderr, "mem case %u: drop %u expected %d, got %d\n", i, j, cases[i].events[j], events[j]);
            return 2;
        }
        joky_runtime_error_clear();
    }
    return 0;
}
