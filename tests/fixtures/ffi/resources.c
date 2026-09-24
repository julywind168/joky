#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

// One real FILE per test process. Inject errors without invoking undefined
// behavior (in particular, close failure still closes the actual stream).
static FILE *temp_stream;
static int32_t temp_mode;
static int32_t temp_opens;
static int32_t temp_flushes;
static int32_t temp_closes;

void jk_temp_reset(int32_t mode) {
    if (temp_stream != NULL) abort();
    temp_mode = mode;
    temp_opens = temp_flushes = temp_closes = 0;
}
void *jk_temp_open(void) {
    if (temp_stream != NULL) abort();
    temp_opens++;
    if (temp_mode & 1) return NULL;
    temp_stream = tmpfile();
    if (temp_stream == NULL) abort(); // Host setup failure, not the injected case.
    return temp_stream;
}
int32_t jk_temp_flush(void *handle) {
    if (handle == NULL || handle != temp_stream) abort();
    temp_flushes++;
    int result = fflush(temp_stream);
    return (temp_mode & 2) ? EOF : result;
}
int32_t jk_temp_close(void *handle) {
    if (handle == NULL || handle != temp_stream) abort();
    temp_closes++;
    int result = fclose(temp_stream);
    temp_stream = NULL;
    return (temp_mode & 4) ? EOF : result;
}
int32_t jk_temp_opens(void) { return temp_opens; }
int32_t jk_temp_flushes(void) { return temp_flushes; }
int32_t jk_temp_closes(void) { return temp_closes; }
int32_t jk_temp_live(void) { return temp_stream != NULL; }
