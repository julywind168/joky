#ifndef JOKY_RESOURCE_H
#define JOKY_RESOURCE_H
#include "joky_cps.h"
#ifdef __cplusplus
extern "C" {
#endif

/* All callbacks are non-unwinding and run on the task thread. The embedding
 * application pins descriptors and callback code until all owners/tasks close.
 * Every nonnull factory output transfers exactly one release obligation, even
 * on failure. Release is synchronous, infallible, and cannot access Joky data. */
typedef int32_t (*JokyResourceCreateV1)(uint64_t, void **);
typedef void (*JokyResourceReleaseV1)(void *);
typedef struct JokyResourceFactoryV1 {
    uint32_t abi_version;
    JokyResourceCreateV1 create;
    JokyResourceReleaseV1 release;
} JokyResourceFactoryV1;
/* Pending must leave output null. Keep unfinished acquisitions in host_context.
 * Stop must revoke all external access before returning; it runs once after
 * the first poll. Output adoption or release occurs only after stop. */
typedef int32_t (*JokyResourcePollV1)(JokyCpsFrame *, void **);
typedef struct JokyResourceFactoryCpsV1 {
    uint32_t abi_version;
    JokyResourcePollV1 poll;
    JokyBorrowingHostStop stop;
    JokyResourceReleaseV1 release;
} JokyResourceFactoryCpsV1;

#define JOKY_BYTE_REQUEST_MAX_V1 (1024u * 1024u)
typedef struct JokyByteRequestV1 JokyByteRequestV1;
/* The request owns its bytes and holds exclusive permission on the resource.
 * Use only request accessors; frame slots contain private compiler data.
 * The request address/data may be retained until stop returns. Stop must join
 * workers, drain registrations and free host_context before returning.
 * Poll returns 1/count=0 for Pending or 0 with a count <= request length.
 * Read count zero is EOF (zero read limits are rejected). Nonempty write count
 * zero is an error. Empty writes complete without polling. Other statuses are
 * terminal host/runtime failures, not language-level network error values. */
typedef int32_t (*JokyResourceIoPollV1)(JokyCpsFrame *, JokyByteRequestV1 *, size_t *);
typedef void (*JokyResourceIoStopV1)(JokyCpsFrame *, JokyByteRequestV1 *, uint32_t);
typedef struct JokyResourceIoV1 {
    uint32_t abi_version;
    JokyResourceIoPollV1 poll;
    JokyResourceIoStopV1 stop;
} JokyResourceIoV1;
uint8_t *joky_byte_request_data_v1(JokyByteRequestV1 *);
uint64_t joky_byte_request_length_v1(const JokyByteRequestV1 *);
void *joky_byte_request_resource_v1(const JokyByteRequestV1 *);
#ifdef __cplusplus
}
#endif
#endif
