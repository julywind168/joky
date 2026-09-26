#ifndef JOKY_CPS_H
#define JOKY_CPS_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ABI v1. Frames are opaque and must be allocated by this runtime.
 * All operations run on one thread. Do not retain a frame/slot pointer beyond
 * its poll, resize slot storage, or unwind across these functions.
 * Generated entries return JOKY_CPS_COMPLETED, JOKY_CPS_PENDING, or an error code.
 * A pending native entry is polled with the same frame until it completes.
 * Int32 occupies the low 32 bits of a slot, Bool the low 8 bits.
 */
typedef struct JokyCpsFrame JokyCpsFrame;
typedef int32_t (*JokyCpsEntry)(JokyCpsFrame *);
enum { JOKY_CPS_COMPLETED = 0, JOKY_CPS_PENDING = 1, JOKY_CPS_INVALID_STATE = 2 };
#define JOKY_CPS_DONE UINT32_MAX

/* Use the slot count reported by AotCompiler::cps_frame_slots for this entry.
 * Allocation failure aborts. Free releases the entire child frame tree.
 */
JokyCpsFrame *joky_cps_frame_new(size_t slot_count);
void joky_cps_frame_free(JokyCpsFrame *frame);
uint32_t joky_cps_state(const JokyCpsFrame *frame);
void joky_cps_set_state(JokyCpsFrame *frame, uint32_t state);
uint64_t joky_cps_result(const JokyCpsFrame *frame);
void joky_cps_set_result(JokyCpsFrame *frame, uint64_t result);

/* Compiler/runtime interface. Slot accesses must stay within slot_count. */
uint64_t *__joky_cps_slots(JokyCpsFrame *frame);
JokyCpsFrame *__joky_cps_child(JokyCpsFrame *frame, size_t slot_count);
void __joky_cps_release_child(JokyCpsFrame *frame);

#ifdef __cplusplus
}
#endif
#endif
