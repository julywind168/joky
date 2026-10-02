#include <stdint.h>
#include <stdio.h>
#include "joky_runtime.h"
#include "joky_cps.h"
static int32_t events[32];
static int count, ready;
void record_drop(int32_t value) { if (count < 32) events[count++] = value; }
int32_t record(int32_t value) { record_drop(value); return value; }
int32_t delayed_value(JokyCpsFrame *frame) {
    if (joky_cps_state(frame) == 0) {
        record_drop((int32_t)__joky_cps_slots(frame)[0]);
        joky_cps_set_state(frame, 1);
        return 1;
    }
    if (!ready) return 1;
    joky_cps_set_result(frame, (uint32_t)((int32_t)__joky_cps_slots(frame)[0] + 1));
    return 0;
}
extern int32_t replace_copy(void);
extern int32_t replace_copy_rhs(void), replace_reference_array(void), replace_bounds(void);
extern int32_t replace_copy_layout(void);
extern int32_t replace_option(void);
extern int32_t replace_element(void);
extern int32_t replace_field(void);
extern int32_t replace_wrapper(void);
extern int32_t replace_enum(void);
extern int32_t replace_failure(void);
extern int32_t replace_rhs_failure(void);
extern int32_t replace_drop_failure(void);
extern int32_t replace_nested(void);
extern int32_t replace_wrapper_element(void);
extern int32_t replace_in_drop(void);
extern int32_t replace_in_element_drop(void);
extern int32_t replace_then_assign(void);
extern int32_t replace_pending(JokyCpsFrame *);
int main(void) {
    struct Case { int32_t (*call)(void); int failure, count; int32_t events[8]; } cases[] = {
        {replace_copy_rhs, 0, 0, {0}},
        {replace_reference_array, 0, 0, {0}},
        {replace_bounds, 1, 1, {1}},
        {replace_copy, 0, 0, {0}},
        {replace_copy_layout, 0, 0, {0}},
        {replace_option, 0, 4, {3, 4, 2, 1}},
        {replace_element, 0, 2, {1, 2}},
        {replace_field, 0, 3, {1, 12, 2}},
        {replace_wrapper, 0, 2, {1, 2}},
        {replace_enum, 0, 3, {1, 2, 3}},
        {replace_failure, 1, 2, {1, 2}},
        {replace_rhs_failure, 1, 2, {2, 1}},
        {replace_drop_failure, 1, 2, {1, 2}},
        {replace_nested, 0, 3, {2, 1, 3}},
        {replace_wrapper_element, 0, 2, {1, 2}},
        {replace_in_drop, 0, 3, {10, 1, 2}},
        {replace_in_element_drop, 0, 3, {10, 1, 2}},
        {replace_then_assign, 0, 4, {2, 1, 3, 4}},
    };
    for (unsigned i = 0; i < sizeof(cases) / sizeof(cases[0]); i++) {
        count = 0;
        int32_t result = cases[i].call();
        if (cases[i].failure != !!joky_runtime_error_code() || (!cases[i].failure && result != 42)) {
            fprintf(stderr, "Memory replace case %u: result %d, error %d\n", i, result, joky_runtime_error_code());
            return 1;
        }
        if (count != cases[i].count) return 2;
        for (int j = 0; j < count; j++) if (events[j] != cases[i].events[j]) return 3;
        joky_runtime_error_clear();
    }
    count = 0;
    JokyCpsFrame *pending = joky_cps_frame_new(PENDING_SLOTS);
    JokyCpsFrame *cancelled = joky_cps_frame_new(PENDING_SLOTS);
    JokyCpsFrame *failed = joky_cps_frame_new(PENDING_SLOTS);
    __joky_cps_slots(pending)[0] = 1;
    for (int i = 0; i < 4; i++) {
        if (replace_pending(pending) != 1 || replace_pending(cancelled) != 1 || replace_pending(failed) != 1) return 4;
        if (count != 6) return 5;
    }
    joky_cps_frame_free(cancelled);
    ready = 1;
    if (replace_pending(pending) != 0 || joky_cps_result(pending) != 42) return 6;
    if (count != 6) return 7;
    if (replace_pending(failed) != JOKY_CPS_RUNTIME_ERROR || !joky_runtime_error_code()) return 8;
    joky_cps_frame_free(pending);
    joky_cps_frame_free(failed);
    return 0;
}
