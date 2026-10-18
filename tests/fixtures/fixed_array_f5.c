#include "bindings.h"
#include "model.h"
#include <string.h>
static int selected, commands, checks, drops, rotations, invalid;
void model_reset(int32_t index) {
    selected=index;
    commands=checks=drops=rotations=invalid=0;
}
void model_drop(void) { ++drops; }
int32_t model_progress(void) { return commands+checks+drops+rotations; }
void model_command(int32_t (*out)[4], const int32_t *step) {
    if (*step<0 || *step>=MODEL_ROUNDS || *step!=commands) {invalid=1; return;}
    memcpy(*out, model_commands[selected][*step], sizeof *out);
    ++commands;
}
int32_t model_check(const int32_t (*value)[8], const int32_t (*saved)[8], const int32_t *step) {
    if (*step<0 || *step>=MODEL_ROUNDS || *step!=checks || commands!=*step+1 || drops!=commands ||
        rotations!=model_rotations[selected][*step]) {invalid=1; return 0;}
    for (int i=0; i<8; ++i) {
        if ((*value)[i]!=model_values[selected][*step][0][i] ||
            (*saved)[i]!=model_values[selected][*step][1][i]) {invalid=1; return 0;}
    }
    ++checks;
    return 1;
}
/* Deliberately use a first-element API only inside this exact-prototype wrapper. */
static void rotate_elements(int32_t *value, size_t count, int32_t delta) {
    const int32_t first=value[0];
    for (size_t i=0; i+1<count; ++i) value[i]=(value[i+1]+delta)%10000;
    value[count-1]=(first+delta)%10000;
}
int32_t model_rotate(int32_t (*value)[8], const int32_t *delta) {
    rotate_elements(*value, 8, *delta);
    ++rotations;
    return -7;
}
int32_t model_clean(void) {
    /* A cancelled/failed prefix may stop before command entry or after its check. */
    return !invalid && checks<=commands && drops>=commands && drops<=commands+1;
}
int32_t model_complete(int32_t rounds) {
    return model_clean() && commands==rounds && checks==rounds && drops==rounds;
}
