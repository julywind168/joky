#ifndef JOKY_FS_H
#define JOKY_FS_H
#include "joky_cps.h"
#ifdef __cplusplus
extern "C" {
#endif
/* Nonblocking file service ABI v1. See docs/file-runtime.md, section 10.3.
 * Callbacks never unwind/reenter Joky. Submit copies input before returning.
 * A background operation retains its own context/wake references. Stop revokes
 * delivery and never joins. Release schedules physical close; it cannot block.
 * Only the task owner calls submit/poll/stop; releases/wakes may cross threads.
 * A result transfers file_token independently of result_token, even on failure.
 * Every non-NULL output must be reclaimable after a protocol error.
 * Hosts bound files/operations/storage and reserve close capacity on open.
 * File operations may carry invalid limits/sizes: check logical Closed first,
 * then enforce read 1..1048576, write <=1048576, and seek origin <=2. Reject
 * invalid requests before copying data or producing any I/O side effects.
 * Context must retain callback code until all background work has drained.
 */
typedef struct JokyFileWakeV1 {
    void *context;
    void (*retain)(void *);
    void (*release)(void *);
    void (*wake)(void *);
} JokyFileWakeV1;
typedef struct JokyFileInputV1 {
    uint32_t operation_code; /* 1 open, 2 read, 3 write, 4 seek, 5 sync, 6 close */
    void *file_token;
    const uint8_t *path;
    uint64_t path_len, flags;
    const uint8_t *bytes;
    uint64_t bytes_len, limit;
    uint32_t origin;
    uint64_t offset_bits;
} JokyFileInputV1;
typedef struct JokyFileResultV1 {
    void *result_token;
    uint32_t error_code; /* 0 success; 1..10 file errors */
    const uint8_t *message;
    uint64_t message_len;
    void *file_token;
    const uint8_t *bytes;
    uint64_t bytes_len, count;
} JokyFileResultV1;
typedef struct JokyFileServiceV1 {
    uint32_t abi_version, struct_size;
    uint64_t reserved;
    void *context;
    void (*retain)(void *);
    void (*release)(void *);
    /* submit: 0 accepted, 1 rejected, 2 protocol failure */
    uint32_t (*submit)(void *, const JokyFileInputV1 *, JokyFileWakeV1, void **, JokyFileResultV1 *);
    /* poll: 0 ready, 1 pending, 2 protocol failure */
    uint32_t (*poll)(void *, void *, JokyFileResultV1 *);
    void (*stop)(void *, void *, uint32_t);
    void (*release_result)(void *, void *);
    void (*release_file)(void *, void *);
    void (*shutdown)(void *);
    uint32_t (*is_quiescent)(void *);
} JokyFileServiceV1;
/* Same frame/entry contract as joky_cps_block_on. Descriptor is borrowed. */
int32_t joky_fs_block_on_v1(JokyCpsEntry, JokyCpsFrame *, const JokyFileServiceV1 *);
/* Explicit CLI capability: native paths relative to cwd captured at entry. */
int32_t joky_fs_block_on_native(JokyCpsEntry, JokyCpsFrame *);
#ifdef __cplusplus
}
#endif
#endif
