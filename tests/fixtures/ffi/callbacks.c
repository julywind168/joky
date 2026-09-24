#include <pthread.h>
#include <stdint.h>
#include <stdlib.h>

typedef int32_t (*Callback)(void *, int32_t);

int32_t jk_callback_call(Callback callback, void *context, int32_t value) {
    return callback(context, value);
}

struct Job {
    pthread_t thread;
    pthread_mutex_t mutex;
    pthread_cond_t wake;
    int released;
    Callback callback;
    void *context;
    int32_t value;
    int32_t result;
};

static void *run(void *pointer) {
    struct Job *job = pointer;
    pthread_mutex_lock(&job->mutex);
    while (!job->released) pthread_cond_wait(&job->wake, &job->mutex);
    pthread_mutex_unlock(&job->mutex);
    job->result = job->callback(job->context, job->value);
    return NULL;
}

void *jk_callback_start(Callback callback, void *context, int32_t value) {
    struct Job *job = calloc(1, sizeof(*job));
    if (!job) return NULL;
    job->callback = callback;
    job->context = context;
    job->value = value;
    if (pthread_mutex_init(&job->mutex, NULL) != 0) { free(job); return NULL; }
    if (pthread_cond_init(&job->wake, NULL) != 0) {
        pthread_mutex_destroy(&job->mutex); free(job); return NULL;
    }
    if (pthread_create(&job->thread, NULL, run, job) != 0) {
        pthread_cond_destroy(&job->wake); pthread_mutex_destroy(&job->mutex); free(job); return NULL;
    }
    return job;
}

void jk_callback_release(void *pointer) {
    struct Job *job = pointer;
    if (!job) return;
    pthread_mutex_lock(&job->mutex);
    job->released = 1;
    pthread_cond_signal(&job->wake);
    pthread_mutex_unlock(&job->mutex);
}

int32_t jk_callback_join(void *pointer) {
    struct Job *job = pointer;
    if (!job) return -1000;
    jk_callback_release(job);
    pthread_join(job->thread, NULL);
    int32_t result = job->result;
    pthread_cond_destroy(&job->wake);
    pthread_mutex_destroy(&job->mutex);
    free(job);
    return result;
}

static pthread_mutex_t gate = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t changed = PTHREAD_COND_INITIALIZER;
static int arrivals;
void jk_callback_mark(void) {
    pthread_mutex_lock(&gate);
    ++arrivals;
    pthread_cond_broadcast(&changed);
    pthread_mutex_unlock(&gate);
}
void jk_callback_wait(int32_t count) {
    pthread_mutex_lock(&gate);
    while (arrivals < count) pthread_cond_wait(&changed, &gate);
    pthread_mutex_unlock(&gate);
}

int8_t jk_callback_narrow(int8_t (*callback)(void *, uint8_t, int16_t), void *context) {
    return callback(context, 250, -30000);
}

double jk_callback_float(double (*callback)(void *, float, double), void *context) {
    return callback(context, 1.25f, 2.5);
}

int32_t jk_callback_void(void (*callback)(void *, int32_t *), void *context) {
    int32_t value = 0;
    callback(context, &value);
    return value;
}
