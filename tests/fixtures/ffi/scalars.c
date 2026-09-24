#include <stdint.h>
#include <stdlib.h>
#include <string.h>

typedef struct { int32_t x; int32_t y; } jk_point;
static jk_point point = { 7, 11 };
static uint8_t fixed_values[4] = { 3, 5, 7, 11 };
static const char message[] = "joky ffi";
jk_point *jk_point_ptr(void) { return &point; }
jk_point *jk_point_mut_ptr(void) { return &point; }
int32_t jk_point_sum(const jk_point *value) { return value->x + value->y; }
void jk_point_set(jk_point *value, int32_t x, int32_t y) { value->x = x; value->y = y; }
const char *jk_message(void) { return message; }
uint64_t jk_message_length(const char *s) { return (uint64_t)strlen(s); }
uint8_t *jk_fixed_values(void) { return fixed_values; }
int32_t jk_fixed_sum(const uint8_t *value) { return value[0] + value[1] + value[2] + value[3]; }

int8_t jk_i8(int8_t x) { return x; }
uint8_t jk_u8(uint8_t x) { return x; }
int16_t jk_i16(int16_t x) { return x; }
uint16_t jk_u16(uint16_t x) { return x; }
int32_t jk_i32(int32_t x) { return x; }
uint32_t jk_u32(uint32_t x) { return x; }
int64_t jk_i64(int64_t x) { return x; }
uint64_t jk_u64(uint64_t x) { return x; }
float jk_f32(float x) { return x * 2.0f; }
double jk_f64(double x) { return x * 2.0; }
int32_t jk_negative(void) { return -42; }
static int32_t state;
void jk_set(int32_t x) { state = x; }
int32_t jk_get(void) { return state; }

/* More arguments than registers, including mixed floating and integer values. */
double jk_mixed(int8_t a, uint8_t b, int16_t c, uint16_t d,
                int32_t e, uint32_t f, int64_t g, uint64_t h,
                float i, double j, int8_t k, uint8_t l,
                int16_t m, uint16_t n, int32_t o, uint32_t p,
                float q, double r) {
    return (double)a + b + c + d + e + f + g + h + i + j + k + l + m + n + o + p + q + r;
}

const int32_t *jk_null_const(void) { return NULL; }
uint8_t *jk_null_mut(void) { return NULL; }
const char *jk_null_string(void) { return NULL; }
const char *jk_empty_string(void) { return ""; }
const void *jk_null_void(void) { return NULL; }
int32_t jk_accept_nulls(const int32_t *a, uint8_t *b, const char *c,
                        const void *d, void *e, const jk_point *f,
                        const uint8_t (*g)[4]) {
    return a == NULL && b == NULL && c == NULL && d == NULL && e == NULL && f == NULL && g == NULL;
}

/* Validate a Joky-lent string: C must see the full UTF-8 content with the
   hidden terminator exactly past it. Returns 1 on success, 0 on any
   mismatch, -1 for a NULL pointer. */
int32_t jk_cstr_check(const char *s, uint64_t utf8_len) {
    if (s == NULL) { return -1; }
    if (strlen(s) != (size_t)utf8_len) { return 0; }
    return s[utf8_len] == '\0' ? 1 : 0;
}

/* Inbound strings for CStr.to_string(): multibyte UTF-8 and a payload that
   is not valid UTF-8 at all. */
static const char utf8_message[] = "h\xc3\xa9llo w\xc3\xb6rld";
const char *jk_utf8_message(void) { return utf8_message; }
static const char invalid_utf8[] = { 'c', (char)0xFF, 'd', '\0' };
const char *jk_invalid_utf8_string(void) { return invalid_utf8; }

/* Out-parameter patterns through C-heap cells allocated by Joky. */
static int64_t cell_stored = 0;
int32_t jk_cell_take(int64_t *cell) { cell_stored = *cell; return 0; }
int64_t jk_cell_stored(void) { return cell_stored; }
static const char out_payload[] = "out param";
int32_t jk_make_pointer(const char **out) { *out = out_payload; return 0; }
static int64_t handle_payload = 777;
int32_t jk_make_handle(void **out) { *out = &handle_payload; return 0; }
int64_t jk_handle_value(void *handle) { return *(int64_t *)handle; }

/* Synchronous native callback: run libc qsort with a Joky comparator and
   return the sorted triple packed into one number. */
int32_t jk_sort3(int32_t a, int32_t b, int32_t c,
                 int (*compare)(const void *, const void *)) {
    int32_t values[3] = { a, b, c };
    qsort(values, 3, sizeof(int32_t), compare);
    return values[0] * 100 + values[1] * 10 + values[2];
}
