/* CLI executable policy only; the runtime ABI never reads the environment. */
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int joky_startup_workers(int argc, char **argv, uint32_t *workers) {
    const char *value = NULL;
    const char *source = "--task-workers";
    for (int i = 1; i < argc; ++i) {
        if (strcmp(argv[i], "--task-workers") != 0) {
            fprintf(stderr, "Unknown startup argument: %s\n", argv[i]);
            return 1;
        }
        if (value != NULL) {
            fputs("--task-workers was specified more than once\n", stderr);
            return 1;
        }
        if (++i == argc) {
            fputs("--task-workers requires a worker count in 1..=256\n", stderr);
            return 1;
        }
        value = argv[i];
    }
    if (value == NULL) {
        value = getenv("JOKY_TASK_WORKERS");
        source = "JOKY_TASK_WORKERS";
    }
    if (value == NULL) return 0;
    if (*workers == 0) {
        fprintf(stderr, "%s requires a Parallel executable; rebuild with --parallel\n", source);
        return 1;
    }
    uint32_t count = 0;
    const unsigned char *p = (const unsigned char *)value;
    if (*p == '\0') goto invalid;
    for (; *p != '\0'; ++p) {
        if (*p < '0' || *p > '9') goto invalid;
        count = count * 10 + (*p - '0');
        if (count > 256) goto invalid;
    }
    if (count == 0) goto invalid;
    *workers = count;
    return 0;
invalid:
    fprintf(stderr, "%s requires a worker count in 1..=256\n", source);
    return 1;
}
