#include "bindings.h"
#include <stdlib.h>

static int32_t ptr_live = 0;
void *c_ptr_create(void) {
    CPoint *p = malloc(sizeof(*p));
    if (p) { p->x = 19; p->y = 20; ++ptr_live; }
    return p;
}
void *c_ptr_identity(void *p) { return p; }
void *c_ptr_two(void *p, void *q) { return q ? NULL : p; }
void *c_ptr_three(void *p, void *q, void *r) { return q == p && !r ? p : NULL; }
void *c_ptr_four(void *p, const int32_t *a, void *q, const int32_t *b) {
    return p == q && *a + *b == 42 ? p : NULL;
}
void c_ptr_bump(void *p) { c_update(p); }
int32_t c_ptr_read(void *p) { return p ? (int32_t)c_inspect(p) : -1; }
void c_ptr_destroy(void *p) { if (p) { free(p); --ptr_live; } }
int32_t c_ptr_live(void) { return ptr_live; }

int64_t c_inspect(const CPoint *p) { return (int64_t)p->x + p->y; }
void c_update(CPoint *p) { p->x += 1; p->y += 2; }
uint64_t c_packet(Packet *p) {
    if (p->tag != 7 || p->point.x != -4 || p->point.y != INT64_C(9000000000) || p->tail != UINT64_MAX) return 0;
    p->point.x = 35;
    p->point.y = -INT64_C(9000000000);
    p->tail = 9;
    return UINT64_MAX;
}
int32_t c_zero(void) { return -42; }
void c_pair(int32_t *a, int32_t *b) { ++*a; ++*b; }
int32_t c_sum(const int32_t *a, const int32_t *b) { return *a + *b; }
uint32_t c_three(const int32_t *a, const int64_t *b, const uint32_t *c) {
    return *a == -7 && *b == -INT64_C(9000000000) ? *c : 0;
}
uint64_t c_four(const int32_t *a, const int64_t *b, const uint32_t *c, const uint64_t *d) {
    return c_three(a, b, c) == UINT32_MAX ? *d : 0;
}
