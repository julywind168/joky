/* Link against libjoky.a and the platform's Rust native libraries. */
#include "joky_cps.h"
#include <assert.h>

int main(void) {
    JokyCpsFrame *frame = joky_cps_frame_new(2);
    assert(joky_cps_state(frame) == 0);
    uint64_t *slots = __joky_cps_slots(frame);
    slots[0] = 20;
    slots[1] = 22;
    JokyCpsFrame *child = __joky_cps_child(frame, 1);
    __joky_cps_slots(child)[0] = slots[0] + slots[1];
    assert(__joky_cps_child(frame, 1) == child);
    joky_cps_set_result(frame, __joky_cps_slots(child)[0]);
    __joky_cps_release_child(frame);
    joky_cps_set_state(frame, JOKY_CPS_DONE);
    assert(joky_cps_result(frame) == 42);
    assert(joky_cps_state(frame) == JOKY_CPS_DONE);
    /* Scope lowering uses indexed children and releases them together. */
    JokyCpsFrame *first = __joky_cps_child_at(frame, 0, 2);
    JokyCpsFrame *second = __joky_cps_child_at(frame, 1, 1);
    assert(first != NULL && second != NULL && first != second);
    __joky_cps_release_children(frame);
    /* Free must also release any pending child frames. */
    __joky_cps_child(frame, 3);
    joky_cps_frame_free(frame);
    joky_cps_frame_free(NULL);
    return 0;
}
