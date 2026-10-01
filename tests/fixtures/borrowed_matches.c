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
extern int32_t shared_match_nested(void), shared_match_returned(void), shared_match_array(void);
extern int32_t shared_match_loops(void), shared_match_empty(void), shared_match_failure(void);
extern int32_t shared_match_drop_failure(void), shared_match_copy(void), shared_match_zero(void);
extern int32_t shared_match_inference(void), shared_match_reference(void), shared_match_wrapper(void);
extern int32_t shared_match_subtrees(void), shared_match_reborrow(void);
extern int32_t shared_match_modules(void);
extern int32_t shared_match_pending(JokyCpsFrame *);
int main(void) {
    struct Case { int32_t (*call)(void); int failure, count; int32_t events[8]; } cases[] = {
        {shared_match_modules, 0, 0, {0}},
        {shared_match_nested, 0, 5, {90, 1, 10, 2, 3}},
        {shared_match_returned, 0, 4, {3, 10, 1, 2}},
        {shared_match_array, 0, 1, {1}},
        {shared_match_loops, 0, 2, {1, 2}},
        {shared_match_empty, 0, 0, {0}},
        {shared_match_failure, 1, 2, {1, 2}},
        {shared_match_drop_failure, 1, 3, {2, 1, 3}},
        {shared_match_copy, 0, 0, {0}},
        {shared_match_zero, 0, 0, {0}},
        {shared_match_inference, 0, 0, {0}},
        {shared_match_reference, 0, 0, {0}},
        {shared_match_wrapper, 0, 0, {0}},
        {shared_match_subtrees, 0, 2, {1, 2}},
        {shared_match_reborrow, 0, 1, {1}},
    };
    for (unsigned i = 0; i < sizeof(cases) / sizeof(cases[0]); i++) {
        count = 0;
        int32_t result = cases[i].call();
        if (cases[i].failure != !!joky_runtime_error_code() || (!cases[i].failure && result != 42)) {
            fprintf(stderr, "Borrowed match case %u: result %d, error %d\n", i, result, joky_runtime_error_code());
            return 1;
        }
        if (count != cases[i].count) return 2;
        for (int j = 0; j < count; j++) if (events[j] != cases[i].events[j]) return 3;
        joky_runtime_error_clear();
    }
    count = 0;
    JokyCpsFrame *pending = joky_cps_frame_new(PENDING_SLOTS);
    JokyCpsFrame *cancelled = joky_cps_frame_new(PENDING_SLOTS);
    for (int i = 0; i < 4; i++) {
        if (shared_match_pending(pending) != 1 || shared_match_pending(cancelled) != 1) return 4;
        if (count != 4) return 5;
    }
    joky_cps_frame_free(cancelled);
    ready = 1;
    if (shared_match_pending(pending) != 0 || joky_cps_result(pending) != 42) return 6;
    if (count != 4) return 7;
    joky_cps_frame_free(pending);
    return 0;
}
