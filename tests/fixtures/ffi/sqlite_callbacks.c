#include <pthread.h>
#include <stdint.h>
#include <stdlib.h>
#include <sqlite3.h>

typedef int32_t (*JokyCallback)(void *, int, const char *, const char *, int64_t);

struct SqliteJob {
    pthread_t thread;
    JokyCallback callback;
    void *context;
    int result;
};

/* sqlite3_update_hook takes sqlite3_int64 (long long). On LP64, int64_t is
   long, a different type, so the hook must match the header and convert
   when calling the Joky callback (Int64 / int64_t). */
static void forward_update(void *context, int operation, const char *database,
                           const char *table, sqlite3_int64 rowid) {
    struct SqliteJob *job = context;
    (void)job->callback(job->context, operation, database, table, (int64_t)rowid);
}

static void *run_sqlite(void *pointer) {
    struct SqliteJob *job = pointer;
    sqlite3 *db = NULL;
    if (sqlite3_open(":memory:", &db) != SQLITE_OK) {
        job->result = 1;
        if (db) sqlite3_close(db);
        return NULL;
    }
    sqlite3_update_hook(db, forward_update, job);
    job->result = sqlite3_exec(db,
        "create table events(value integer); insert into events values (42);",
        NULL, NULL, NULL);
    sqlite3_close(db);
    return NULL;
}

void *jk_sqlite_async_start(JokyCallback callback, void *context) {
    struct SqliteJob *job = calloc(1, sizeof(*job));
    if (!job) return NULL;
    job->callback = callback;
    job->context = context;
    if (pthread_create(&job->thread, NULL, run_sqlite, job) != 0) {
        free(job);
        return NULL;
    }
    return job;
}

int32_t jk_sqlite_async_join(void *pointer) {
    struct SqliteJob *job = pointer;
    if (!job) return -1;
    pthread_join(job->thread, NULL);
    int result = job->result;
    free(job);
    return result;
}
