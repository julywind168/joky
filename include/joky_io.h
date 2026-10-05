#ifndef JOKY_IO_H
#define JOKY_IO_H
#include "joky_vm.h"
#ifdef __cplusplus
extern "C" {
#endif
/* Synchronously write exactly byte_length UTF-8 bytes, including embedded NULs.
 * The descriptor and bytes remain live and immutable throughout the call.
 * Return 0 on success, -1 on failure; failure records the runtime error.
 * A failed write may have already emitted a prefix. No pointer is retained. */
int32_t joky_io_write_stdout_v2(const JokyText *text);
#ifdef __cplusplus
}
#endif
#endif
