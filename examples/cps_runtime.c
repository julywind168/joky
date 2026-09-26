/* Link against libjoky_vm_interop.a and the platform's Rust native libraries. */
#include "jk_cps.h"
#include <assert.h>

int main(void) {
    JkCpsFrame *frame = jk_cps_frame_new(2);
    assert(jk_cps_state(frame) == 0);
    uint64_t *slots = __jk_cps_slots(frame);
    slots[0] = 20;
    slots[1] = 22;
    JkCpsFrame *child = __jk_cps_child(frame, 1);
    __jk_cps_slots(child)[0] = slots[0] + slots[1];
    assert(__jk_cps_child(frame, 1) == child);
    jk_cps_set_result(frame, __jk_cps_slots(child)[0]);
    __jk_cps_release_child(frame);
    jk_cps_set_state(frame, JK_CPS_DONE);
    assert(jk_cps_result(frame) == 42);
    assert(jk_cps_state(frame) == JK_CPS_DONE);
    /* Free must also release pending child frames. */
    __jk_cps_child(frame, 3);
    jk_cps_frame_free(frame);
    jk_cps_frame_free(NULL);
    return 0;
}
