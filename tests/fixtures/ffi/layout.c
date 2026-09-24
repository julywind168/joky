#include <stdint.h>
#include <stddef.h>
#include <stdio.h>

typedef struct { uint8_t tag; uint32_t number; } Header;
typedef struct Packet {
    uint8_t tag;
    Header headers[3];
    double measure;
    struct Packet *next;
} Packet;

int main(void) {
    printf("%zu %zu %zu %zu\n", sizeof(Header), _Alignof(Header),
           offsetof(Header, tag), offsetof(Header, number));
    printf("%zu %zu %zu %zu %zu %zu\n", sizeof(Packet), _Alignof(Packet),
           offsetof(Packet, tag), offsetof(Packet, headers),
           offsetof(Packet, measure), offsetof(Packet, next));
}
