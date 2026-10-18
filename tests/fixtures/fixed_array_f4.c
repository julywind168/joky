#include "bindings.h"
#include <string.h>

/* Independent ABI assertions also constrain the expected C shapes. */
_Static_assert(sizeof(((Packet *)0)->matrix) == 6 * sizeof(int32_t), "matrix size");
_Static_assert(sizeof(((Packet *)0)->matrix[0]) == 3 * sizeof(int32_t), "row stride");
_Static_assert(offsetof(Packet, matrix) == 4, "matrix offset");
_Static_assert(sizeof(((Packet *)0)->cells[0]) == sizeof(FloatCell), "cell stride");
_Static_assert(_Alignof(Packet) == _Alignof(double), "packet alignment");

int32_t cfa_packet(Packet *p) {
    if (p->lead != 7 || p->tail != UINT16_MAX || p->matrix[1][2] != 10 ||
        p->cells[1].tag != 3 || p->cells[1].x != 1.5f || p->cells[1].y != 2.25) return 0;
    p->matrix[1][2] = 42;
    p->cells[1].y = 42.25;
    return 42;
}
int32_t cfa_matrix(const int32_t (*p)[2][3]) {
    return (*p)[0][0] == 10 && (*p)[1][2] == 42 ? 42 : 0;
}
int32_t cfa_cells(FloatCell (*p)[2]) {
    if ((*p)[1].y != 42.25 || (*p)[0].x != 1.5f) return 0;
    (*p)[0].x = 21.5f;
    (*p)[1].tag = 9;
    return 42;
}
/* These real APIs take element pointers and size_t lengths. Only the wrappers
   below cross the Joky ABI, with exact pointer-to-array prototypes. */
static int32_t read_elements(const int32_t *values, size_t length) {
    int32_t total = 0;
    for (size_t i = 0; i < length; ++i) total += values[i];
    return total;
}
static int32_t write_elements(int32_t *values, size_t length) {
    if (length) values[0] += 2;
    return -1;
}
int32_t cfa_read(const int32_t (*p)[4], const uint64_t *length) {
    if (*length > 4) return -1;
    return read_elements(*p, (size_t)*length);
}
int32_t cfa_write(int32_t (*p)[4], const uint64_t *length) {
    if (*length > 4) return -2;
    return write_elements(*p, (size_t)*length);
}
int32_t cfa_address(const int32_t (*p)[4], const int32_t *first) {
    return *p == first ? 42 : 0;
}
int32_t cfa_bytes(uint8_t (*p)[8], uint64_t *length) {
    const uint8_t utf8[] = {0xc3, 0xa9, 0xe4, 0xb8, 0xad};
    memcpy(*p, utf8, sizeof utf8);
    *length = sizeof utf8;
    return 0;
}
int32_t cfa_byte_read(const uint8_t (*p)[8], const uint64_t *length) {
    if (*length > 8) return -1;
    return *length == 5 && (*p)[0] == 0xc3 && (*p)[4] == 0xad ? 42 : 0;
}
int32_t cfa_floats(float (*a)[2], double (*b)[2]) {
    if ((*a)[1] != 1.5f || (*b)[0] != 2.25) return 0;
    (*a)[1] = 21.5f;
    (*b)[0] = 20.5;
    return 42;
}
int32_t cfa_signed(int8_t (*a)[2], int16_t (*b)[2], int32_t (*c)[2], int64_t (*d)[2]) {
    if ((*a)[0] != INT8_MIN || (*b)[0] != INT16_MIN || (*c)[0] != INT32_MIN || (*d)[0] != INT64_MIN) return 0;
    (*a)[1] = 1; (*b)[1] = 2; (*c)[1] = 3; (*d)[1] = 36;
    return 42;
}
int32_t cfa_unsigned(uint8_t (*a)[2], uint16_t (*b)[2], uint32_t (*c)[2], uint64_t (*d)[2]) {
    if ((*a)[0] != UINT8_MAX || (*b)[0] != UINT16_MAX || (*c)[0] != UINT32_MAX || (*d)[0] != UINT64_MAX) return 0;
    (*a)[1] = 1; (*b)[1] = 2; (*c)[1] = 3; (*d)[1] = 36;
    return 42;
}

int32_t cfa_packets(Packet (*p)[2]) {
    if ((*p)[1].matrix[1][2] != 42 || (*p)[1].cells[0].x != 21.5f) return 0;
    (*p)[1].matrix[0][0] = 42;
    return 42;
}
