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
 * Int32/UInt32 occupy the low 32 bits, Int64/UInt64 all 64 bits, Bool the low 8 bits.
 * UInt32 is zero-extended when written to a slot. Signedness remains part of the signature.
 */
typedef struct JokyCpsFrame JokyCpsFrame;
typedef int32_t (*JokyCpsEntry)(JokyCpsFrame *);
enum { JOKY_CPS_COMPLETED = 0, JOKY_CPS_PENDING = 1, JOKY_CPS_INVALID_STATE = 2,
       JOKY_CPS_RUNTIME_ERROR = 3 };
#define JOKY_CPS_DONE UINT32_MAX
#define JOKY_CPS_FAILED (UINT32_MAX - 1)

/* Use the slot count reported by AotCompiler::cps_frame_slots for this entry.
 * Allocation failure aborts. Free releases the entire child frame tree and
 * task arena. Generated terminal entries release arena ownership before
 * returning, while preserving scalar results for inspection.
 */
JokyCpsFrame *joky_cps_frame_new(size_t slot_count);
void joky_cps_frame_free(JokyCpsFrame *frame);
uint32_t joky_cps_state(const JokyCpsFrame *frame);
void joky_cps_set_state(JokyCpsFrame *frame, uint32_t state);
uint64_t joky_cps_result(const JokyCpsFrame *frame);
void joky_cps_set_result(JokyCpsFrame *frame, uint64_t result);

/* Optional operation-owned context. The runtime does not free this pointer. */
void *joky_cps_host_context(const JokyCpsFrame *frame);
void joky_cps_set_host_context(JokyCpsFrame *frame, void *context);

/* Borrowing host ABI v1, separate from legacy JokyCpsEntry imports.
 * A source declaration with @suspends and reference parameters imports the
 * constant descriptor __joky_cps_borrow_v1_<symbol> (including any AOT prefix).
 * Parameters are 1-4 references to fixed-width integers or #repr(C) records;
 * results are integers or Unit, following the CPS slot conventions above.
 * poll may retain object addresses while pending, NEVER frame/slot addresses.
 * stop is mandatory and runs exactly once for each operation whose poll began,
 * including partial startup failure. Before it returns, all external access
 * and queued callbacks must have stopped and operation state must be disposed.
 * It is synchronous, idempotent, non-unwinding, and cannot reenter this operation
 * or conflict with other Joky accesses. References cannot escape, be freed,
 * reallocated, or accessed outside their layout/declared read-write permissions.
 * Callbacks execute on the task thread. External workers may only access the
 * contracted objects and must be stopped/joined by stop; no frame or arena sharing.
 * Keep descriptor/callback code and dependencies alive until all frames release.
 * A terminal poll stops access before its caller continues. Frame free, task
 * failure, and race cancellation stop pending operations before freeing storage.
 */
#define JOKY_BORROWING_HOST_ABI_V1 1u
enum { JOKY_HOST_STOP_COMPLETED = 0, JOKY_HOST_STOP_FAILED = 1,
       JOKY_HOST_STOP_CANCELLED = 2 };
typedef void (*JokyBorrowingHostStop)(JokyCpsFrame *, uint32_t reason);
typedef struct JokyBorrowingHostV1 {
    uint32_t abi_version;
    JokyCpsEntry poll;
    JokyBorrowingHostStop stop;
} JokyBorrowingHostV1;
int32_t joky_cps_poll_borrowing_v1(JokyCpsFrame *, const JokyBorrowingHostV1 *);

/* Compiler/runtime interface. Slot accesses must stay within slot_count. */
uint64_t *__joky_cps_slots(JokyCpsFrame *frame);
JokyCpsFrame *__joky_cps_child(JokyCpsFrame *frame, size_t slot_count);
void __joky_cps_release_child(JokyCpsFrame *frame);
JokyCpsFrame *__joky_cps_child_at(JokyCpsFrame *frame, size_t index, size_t slot_count);
void __joky_cps_release_children(JokyCpsFrame *frame);
void *__joky_cps_arena(JokyCpsFrame *frame);
void __joky_cps_finish(JokyCpsFrame *frame);

#ifdef __cplusplus
}
#endif
#endif
