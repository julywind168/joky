#ifndef JOKY_CPS_H
#define JOKY_CPS_H

#include <stddef.h>
#include <stdint.h>
#include "joky_vm.h"

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
/* Scalar Script child: slots are VM handle, JokyText name pointer, Int32 input.
 * Requires an active executor; owns its continuation until completion/cancel.
 * Errors use the CPS runtime channel, not joky_vm_error_code. */
int32_t joky_vm_call_async_int32_v2(JokyCpsFrame *);

/* Text v2 root entries and slot counts come from compiler metadata.
 * Copy String inputs before the first poll; never write owning handles by hand.
 * Text results are captured before task cleanup. Numeric result access returns
 * zero for text roots. take returns 1 when no completed result is available. */
int32_t joky_cps_set_text_input_v2(JokyCpsFrame *, size_t index, const uint8_t *, uint64_t);
int32_t joky_cps_take_text_result_v2(JokyCpsFrame *, JokyOwnedTextV2 *);
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

/* Drive a fresh root (state zero, initialized arguments, complete frame size)
 * on its owning thread. Keep all reachable code and host data alive until return.
 * Supports joky/task timers, registered I/O readiness and retained TaskWakers.
 * Unsupported external waits fail instead of busy polling. Rust host adapters
 * capture the current executor's waker/reactor; frame loops use nonblocking poll.
 * Returns zero only on DONE. Failures populate joky_runtime_error_*.
 * Releases children and arena on every terminal path, preserves scalar result,
 * and leaves the frame allocated. The caller must free it and never resume it.
 */
int32_t joky_cps_block_on(JokyCpsEntry entry, JokyCpsFrame *frame);

/* Prepare the shared fixed DNS pool before manually polling native DNS tasks.
 * VM module loading and joky_cps_block_on initialize it automatically.
 * initialize returns 0 on success; errors use joky_runtime_error_*.
 * shutdown permanently closes admission and never waits for running resolution.
 * Cancel/destroy task owners first. It returns 1 only after workers exited and
 * were joined. Keep this library loaded until then; OS lookup has no bounded
 * termination guarantee. Reinitialization after shutdown is unsupported. */
int32_t joky_dns_initialize(void);
int32_t joky_dns_shutdown(void);

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
/* Local static-group adapters for checked generated code only. All captures are
 * prepared before open (kind: Join=0, Race=1). next returns a source index,
 * -1 for Pending, -2 for a selected result, or -3 with a runtime error.
 * Selection is not a cleanup barrier: release_children still stops losers
 * after result ownership has been registered. No cross-thread frame access.
 * open/polled return 0 on success, 2 with a runtime error on failure.
 * polled accepts only the active child's Complete=0 or Pending=1 status. */
int32_t __joky_cps_concurrent_open_v1(JokyCpsFrame *frame, uint32_t kind, size_t count);
int64_t __joky_cps_concurrent_next_v1(JokyCpsFrame *frame);
int32_t __joky_cps_concurrent_polled_v1(JokyCpsFrame *frame, size_t index, int32_t status);
/* Independent local result packets. Prepare every packet before polling; take
 * succeeds once after selection. Generated code copies lanes and transfers its
 * cleanup registration before release_children destroys packet storage. */
JokyCpsFrame *__joky_cps_concurrent_result_v1(JokyCpsFrame *frame, size_t index, size_t slots);
int64_t __joky_cps_concurrent_next_result_v1(JokyCpsFrame *frame);
JokyCpsFrame *__joky_cps_concurrent_take_v1(JokyCpsFrame *frame, size_t index);
void *__joky_cps_arena(JokyCpsFrame *frame);
void __joky_cps_finish(JokyCpsFrame *frame);

#ifdef __cplusplus
}
#endif
#endif
