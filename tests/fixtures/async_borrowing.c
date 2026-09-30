#include "async_borrowing.h"
#include "joky_runtime.h"
#include <assert.h>
#include <stdlib.h>

#ifdef ASYNC_VM_TEST
/* In-process tests use the executable's runtime (including its error TLS). */
typedef struct AsyncRuntime {
    uint64_t *(*slots)(JokyCpsFrame *);
    void *(*context)(const JokyCpsFrame *);
    void (*set_context)(JokyCpsFrame *, void *);
    void (*set_result)(JokyCpsFrame *, uint64_t);
    void (*clear_error)(void);
    void (*division_error)(size_t, size_t);
} AsyncRuntime;
static AsyncRuntime api;
void async_bind_runtime(const AsyncRuntime *runtime) { api = *runtime; }
#define __joky_cps_slots api.slots
#define joky_cps_host_context api.context
#define joky_cps_set_host_context api.set_context
#define joky_cps_set_result api.set_result
#define joky_runtime_error_clear api.clear_error
#define __joky_runtime_division_by_zero api.division_error
#endif

/* Deterministic external event source. Operations keep object pointers in heap
 * state, independently of polls. Removing an operation drains its future ticks. */
typedef struct Operation {
    AsyncPoint *point;
    int writable;
    struct Operation *next;
} Operation;
static Operation *operations;
static int read_ready, timer_ready, starts, stops[3], parents;

void async_reset(void) {
    assert(!operations);
    read_ready = timer_ready = starts = parents = 0;
    for (int i = 0; i < 3; ++i) stops[i] = 0;
}
void async_ready(int32_t read, int32_t timer) { read_ready = read; timer_ready = timer; }
void async_tick(void) {
    for (Operation *op = operations; op; op = op->next) {
        if (op->writable) ++op->point->value;
        else assert(op->point->value == 42);
    }
}
int32_t async_count(int32_t which) {
    if (which == 0) return starts;
    if (which >= 1 && which <= 3) return stops[which - 1];
    if (which == 4) return parents;
    int count = 0;
    for (Operation *op = operations; op; op = op->next) ++count;
    return count;
}
static int32_t poll_io(JokyCpsFrame *frame, int writable) {
    Operation *op = joky_cps_host_context(frame);
    if (!op) {
        op = malloc(sizeof(*op));
        assert(op);
        op->point = (AsyncPoint *)(uintptr_t)__joky_cps_slots(frame)[0];
        op->writable = writable;
        op->next = operations; operations = op;
        joky_cps_set_host_context(frame, op);
        ++starts;
    }
    if (op->point->mode == -1) return 7; /* failure after partial startup */
    if (op->point->mode == -3) {
        __joky_runtime_division_by_zero(10, 11);
        return JOKY_CPS_PENDING; /* native error overrides apparent Pending */
    }
    if (op->point->mode != 2 && !read_ready) return JOKY_CPS_PENDING;
    if (op->point->mode == -2) return 7;
    joky_cps_set_result(frame, (uint32_t)op->point->value);
    return JOKY_CPS_COMPLETED;
}
static int32_t poll_write(JokyCpsFrame *frame) { return poll_io(frame, 1); }
static int32_t poll_read(JokyCpsFrame *frame) { return poll_io(frame, 0); }
static void stop_io(JokyCpsFrame *frame, uint32_t reason) {
    assert(reason <= JOKY_HOST_STOP_CANCELLED);
    Operation *op = joky_cps_host_context(frame);
    if (!op) return;
    assert(op->point->value >= 0); /* storage must still be alive during stop */
    Operation **cursor = &operations;
    while (*cursor && *cursor != op) cursor = &(*cursor)->next;
    assert(*cursor == op);
    *cursor = op->next;
    ++stops[reason];
    free(op);
    joky_cps_set_host_context(frame, NULL);
    /* Cleanup must not erase a parent failure's error channel. */
    joky_runtime_error_clear();
}
const JokyBorrowingHostV1 __joky_cps_borrow_v1_host_io = {1, poll_write, stop_io};
const JokyBorrowingHostV1 __joky_cps_borrow_v1_host_read = {1, poll_read, stop_io};
static int32_t poll_four(JokyCpsFrame *frame) {
    uint64_t *s = __joky_cps_slots(frame);
    assert(*(int32_t *)(uintptr_t)s[0] == -1);
    assert(*(int64_t *)(uintptr_t)s[1] == -2);
    assert(*(uint32_t *)(uintptr_t)s[2] == 3);
    joky_cps_set_result(frame, *(uint64_t *)(uintptr_t)s[3]);
    ++starts;
    return JOKY_CPS_COMPLETED;
}
static void stop_four(JokyCpsFrame *frame, uint32_t reason) {
    (void)frame; assert(reason == JOKY_HOST_STOP_COMPLETED); ++stops[reason];
}
const JokyBorrowingHostV1 __joky_cps_borrow_v1_host_four = {1, poll_four, stop_four};
int32_t timeout(JokyCpsFrame *frame) { (void)frame; return timer_ready ? 0 : 1; }
int32_t after_stop(int32_t value) { assert(!operations); ++parents; return value; }
