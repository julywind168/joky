#ifndef JOKY_TASK_H
#define JOKY_TASK_H

#include "joky_cps.h"

#ifdef __cplusplus
extern "C" {
#endif

/* CPS primitives, polled on the frame's owning thread. Use a fresh runtime frame
 * with at least the stated slots, preserve it while pending, and never poll after
 * completion. No frame pointer is retained and no background worker is created.
 * Freeing the frame cancels its wake source. Both return Unit (result bits zero).
 */
int32_t joky_task_yield_now(JokyCpsFrame *frame); /* No parameter slots. */
/* Slot 0: UInt64 milliseconds. Deadline uses a monotonic clock. */
int32_t joky_task_sleep_ms(JokyCpsFrame *frame);

#ifdef __cplusplus
}
#endif
#endif
