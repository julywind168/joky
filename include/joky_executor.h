#ifndef JOKY_EXECUTOR_H
#define JOKY_EXECUTOR_H
#include <stddef.h>
#include <stdint.h>
#ifdef __cplusplus
extern "C" {
#endif

typedef struct JokyCpsFrame JokyCpsFrame;
/* Version 1: mode 0 = Local (workers = 0), mode 1 = Parallel (1..256).
 * In Parallel, max_tasks bounds all live root and child tasks. Local uses the
 * existing blocking driver limits (max_tasks is ignored after validation).
 * No environment overrides. */
typedef struct JokyExecutionConfigV1 {
    uint32_t version, size, mode, workers;
    size_t max_tasks;
} JokyExecutionConfigV1;
/* Compiler-authored evidence for a static zero-argument CPS root. result_kind:
 * 0 = Unit, 1 = Int32. reserved must be zero. Not an arbitrary FFI descriptor. */
typedef struct JokyTaskMainV1 {
    uint32_t version, size;
    int32_t (*entry)(JokyCpsFrame *);
    size_t frame_slots;
    uint32_t result_kind, reserved;
} JokyTaskMainV1;
/* Unsafe embedding contract: descriptor must match the compiler-checked entry
 * and all transitive capabilities, linked to this runtime ABI. Code and its
 * dependencies must remain live through this call. Output is written only on
 * success; -1 records a runtime error on the calling thread. Never call from a
 * task worker. This is a blocking standalone driver, not a raw task submission API. */
int32_t __joky_task_main_v1(const JokyExecutionConfigV1 *, const JokyTaskMainV1 *, uint64_t *output);
#ifdef __cplusplus
}
#endif
#endif
