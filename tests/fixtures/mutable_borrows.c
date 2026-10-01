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
extern int32_t mutable_match_empty(void);
extern int32_t mutable_match_control_flow(void);
extern int32_t mutable_borrow_nested_array(void);
extern int32_t mutable_borrow_drop_failure(void);
extern int32_t mutable_element(void);
extern int32_t mutable_match_nested(void);
extern int32_t mutable_match_array(void);
extern int32_t mutable_match_copy(void);
extern int32_t mutable_match_generic(void);
extern int32_t mutable_element_wrapper(void);
extern int32_t mutable_match_scalar_return(void);
extern int32_t mutable_match_nested_enum(void);
extern int32_t mutable_borrow_bounds(void);
extern int32_t mutable_borrow_failure(void);
extern int32_t mutable_borrow_modules(void);
extern int32_t mutable_pending(JokyCpsFrame *);
int main(void) {
    struct Case { int32_t (*call)(void); int failure, count; int32_t events[8]; } cases[] = {
        {mutable_borrow_modules, 0, 0, {0}},
        {mutable_match_empty, 0, 0, {0}},
        {mutable_match_control_flow, 0, 1, {3}},
        {mutable_borrow_nested_array, 0, 0, {0}},
        {mutable_borrow_drop_failure, 1, 2, {2, 3}},
        {mutable_element, 0, 2, {2, 21}},
        {mutable_match_nested, 0, 2, {10, 20}},
        {mutable_match_array, 0, 1, {41}},
        {mutable_match_copy, 0, 0, {0}},
        {mutable_match_generic, 0, 0, {0}},
        {mutable_element_wrapper, 0, 0, {0}},
        {mutable_match_scalar_return, 0, 1, {1}},
        {mutable_match_nested_enum, 0, 1, {41}},
        {mutable_borrow_bounds, 1, 1, {1}},
        {mutable_borrow_failure, 1, 1, {2}},
    };
    for (unsigned i = 0; i < sizeof(cases) / sizeof(cases[0]); i++) {
        count = 0;
        int32_t result = cases[i].call();
        if (cases[i].failure != !!joky_runtime_error_code() || (!cases[i].failure && result != 42)) {
            fprintf(stderr, "Mutable borrow case %u: result %d, error %d\n", i, result, joky_runtime_error_code());
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
        if (mutable_pending(pending) != 1 || mutable_pending(cancelled) != 1 || mutable_pending(failed) != 1) return 4;
        if (count != 6) return 5;
    }
    joky_cps_frame_free(cancelled);
    ready = 1;
    if (mutable_pending(pending) != 0 || joky_cps_result(pending) != 42) return 6;
    if (count != 6) return 7;
    if (mutable_pending(failed) != JOKY_CPS_RUNTIME_ERROR || !joky_runtime_error_code()) return 8;
    joky_cps_frame_free(pending);
    joky_cps_frame_free(failed);
    return 0;
}
