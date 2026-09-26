#ifndef JK_CPS_H
#define JK_CPS_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ABI v1. Frames are opaque and must be allocated by this runtime.
 * All operations run on one thread. Do not retain a frame/slot pointer beyond
 * its poll, resize slot storage, or unwind across these functions.
 * Generated entries return JK_CPS_COMPLETED, JK_CPS_PENDING, or an error code.
 * A pending native entry is polled with the same frame until it completes.
 * Int32 occupies the low 32 bits of a slot, Bool the low 8 bits.
 */
typedef struct JkCpsFrame JkCpsFrame;
typedef int32_t (*JkCpsEntry)(JkCpsFrame *);
enum { JK_CPS_COMPLETED = 0, JK_CPS_PENDING = 1, JK_CPS_INVALID_STATE = 2 };
#define JK_CPS_DONE UINT32_MAX

/* Use the slot count reported by AotCompiler::cps_frame_slots for this entry.
 * Allocation failure aborts. Free releases the entire child frame tree.
 */
JkCpsFrame *jk_cps_frame_new(size_t slot_count);
void jk_cps_frame_free(JkCpsFrame *frame);
uint32_t jk_cps_state(const JkCpsFrame *frame);
void jk_cps_set_state(JkCpsFrame *frame, uint32_t state);
uint64_t jk_cps_result(const JkCpsFrame *frame);
void jk_cps_set_result(JkCpsFrame *frame, uint64_t result);

/* Compiler/runtime interface. Slot accesses must stay within slot_count. */
uint64_t *__jk_cps_slots(JkCpsFrame *frame);
JkCpsFrame *__jk_cps_child(JkCpsFrame *frame, size_t slot_count);
void __jk_cps_release_child(JkCpsFrame *frame);

#ifdef __cplusplus
}
#endif
#endif
