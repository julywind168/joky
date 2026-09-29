#include "bindings.h"

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
