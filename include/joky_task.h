#ifndef JOKY_TASK_H
#define JOKY_TASK_H

#include "joky_cps.h"

#ifdef __cplusplus
extern "C" {
#endif

/* Synchronous clock reads. Domain 0 is the process system monotonic clock;
 * executor domains are process-local and must not be serialized. Check the
 * runtime error after reads. Ticks are nanoseconds since the domain origin.
 * Wall time is Unix microseconds, rounded toward the preceding microsecond. */
uint64_t joky_time_clock(void);
uint64_t joky_time_ticks(uint64_t clock);
int64_t joky_time_unix_micros(void);

/* CPS primitives, polled on the frame's owning thread. Use a fresh runtime frame
 * with at least the stated slots, preserve it while pending, and never poll after
 * completion. No frame pointer is retained and no background worker is created.
 * Freeing the frame cancels its wake source. All return Unit (result bits zero).
 */
int32_t joky_task_yield_now(JokyCpsFrame *frame); /* No parameter slots. */
/* Slot 0: UInt64 nanoseconds. Deadline uses the current monotonic clock. */
int32_t joky_task_sleep_ns(JokyCpsFrame *frame);
/* Slots 0/1: clock identity and absolute nanosecond ticks from joky/time.Instant.
 * Requires the same clock domain at every poll. */
int32_t joky_task_sleep_until(JokyCpsFrame *frame);

#ifdef __cplusplus
}
#endif
#endif
