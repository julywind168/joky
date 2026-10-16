#include "joky_fs.h"
#include "joky_runtime.h"
extern int32_t missing(JokyCpsFrame *);
static unsigned references, calls, unexpected;
static void retain(void *context) { (void)context; references++; }
static void release(void *context) { (void)context; references--; }
static uint32_t submit(void *context, const JokyFileInputV1 *input,
                       JokyFileWakeV1 wake, void **operation, JokyFileResultV1 *result) {
    (void)context; (void)wake; (void)operation;
    if (input->operation_code != 1 || input->path_len != 6 || input->flags != 1) unexpected++;
    calls++;
    result->error_code = 1;
    return 1;
}
static uint32_t poll_file(void *context, void *operation, JokyFileResultV1 *result) {
    (void)context; (void)operation; (void)result; unexpected++; return 2;
}
static void stop(void *context, void *operation, uint32_t reason) {
    (void)context; (void)operation; (void)reason; unexpected++;
}
static void release_token(void *context, void *token) { (void)context; (void)token; unexpected++; }
static void shutdown_file(void *context) { (void)context; }
static uint32_t quiet(void *context) { (void)context; return 1; }
int main(void) {
    JokyCpsFrame *frame = joky_cps_frame_new(FRAME_SLOTS);
    int status = joky_cps_block_on(missing, frame);
    int ok = status == 0 && joky_cps_result(frame) == 42;
    joky_cps_frame_free(frame);
    JokyFileServiceV1 service = {
        1, sizeof(JokyFileServiceV1), 0, &references,
        retain, release, submit, poll_file, stop, release_token,
        release_token, shutdown_file, quiet
    };
    frame = joky_cps_frame_new(FRAME_SLOTS);
    status = joky_fs_block_on_v1(missing, frame, &service);
    ok = ok && status == 0 && joky_cps_result(frame) == 42;
    joky_cps_frame_free(frame);
    ok = ok && references == 0 && calls == 1 && unexpected == 0;
    service.abi_version = 2;
    frame = joky_cps_frame_new(FRAME_SLOTS);
    status = joky_fs_block_on_v1(missing, frame, &service);
    ok = ok && status != 0 && joky_runtime_error_code() != 0 && references == 0;
    joky_cps_frame_free(frame);
    joky_runtime_error_clear();
    return ok ? 0 : 1;
}
