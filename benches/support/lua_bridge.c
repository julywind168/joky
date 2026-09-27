/* Optional benchmark adapter. Compile against the installed Lua headers so
 * Rust never assumes lua_Integer's ABI or the layout of lua_State. */
#include <lua.h>
#include <lauxlib.h>
#include <limits.h>
#include <stdint.h>
#include <stdio.h>

#if LUA_VERSION_NUM < 503 || LUA_VERSION_NUM >= 600
#error "The benchmark requires Lua 5.3 or newer from the Lua 5 series"
#endif
_Static_assert(sizeof(lua_Integer) * CHAR_BIT == 64,
               "The Lua-native benchmark requires 64-bit Lua integers");

enum { KERNEL_COUNT = 6 };

static void message(char *error, size_t capacity, const char *text) {
    snprintf(error, capacity, "%s", text);
}

static void lua_error_message(lua_State *state, char *error, size_t capacity) {
    /* Do not coerce non-string error objects, which could allocate. */
    const char *text = lua_type(state, -1) == LUA_TSTRING
        ? lua_tostring(state, -1) : "Lua returned a non-string error";
    message(error, capacity, text);
}

static int check_version(lua_State *state) {
    luaL_checkversion(state);
    return 0;
}

void *joky_bench_lua_create(const char *source, size_t length,
                          char *error, size_t capacity) {
    lua_State *state = luaL_newstate();
    if (!state) {
        message(error, capacity, "Could not allocate Lua state");
        return NULL;
    }
    /* Version checks and chunk execution are protected inside C, so Lua
     * errors never longjmp through Rust frames. No standard libraries needed. */
    lua_pushcfunction(state, check_version);
    if (lua_pcall(state, 0, 0, 0) != LUA_OK ||
        luaL_loadbufferx(state, source, length, "@while.lua", "t") != LUA_OK ||
        lua_pcall(state, 0, KERNEL_COUNT, 0) != LUA_OK) {
        lua_error_message(state, error, capacity);
        lua_close(state);
        return NULL;
    }
    for (int index = 1; index <= KERNEL_COUNT; ++index) {
        if (!lua_isfunction(state, index)) {
            message(error, capacity, "Lua source must return six functions");
            lua_close(state);
            return NULL;
        }
    }
    /* Keep functions at fixed stack indices; timed calls use no global lookup.
     * Reserve argument space before timing; every call restores this stack. */
    if (!lua_checkstack(state, 4)) {
        message(error, capacity, "Could not reserve Lua stack");
        lua_close(state);
        return NULL;
    }
    return state;
}

int joky_bench_lua_call(void *opaque, int kernel, int32_t n, int32_t seed,
                       int32_t *result, char *error, size_t capacity) {
    lua_State *state = opaque;
    if (kernel < 0 || kernel >= KERNEL_COUNT) {
        message(error, capacity, "Invalid Lua benchmark kernel");
        return 1;
    }
    lua_pushvalue(state, kernel + 1);
    lua_pushinteger(state, n);
    lua_pushinteger(state, seed);
    if (lua_pcall(state, 2, 1, 0) != LUA_OK) {
        lua_error_message(state, error, capacity);
        lua_settop(state, KERNEL_COUNT);
        return 1;
    }
    if (!lua_isinteger(state, -1)) {
        message(error, capacity, "Lua result is not an integer");
        lua_settop(state, KERNEL_COUNT);
        return 1;
    }
    lua_Integer value = lua_tointeger(state, -1);
    lua_settop(state, KERNEL_COUNT);
    if (value < INT32_MIN || value > INT32_MAX) {
        message(error, capacity, "Lua result is outside signed Int32");
        return 1;
    }
    *result = (int32_t)value;
    return 0;
}

void joky_bench_lua_destroy(void *state) {
    lua_close(state);
}

const char *joky_bench_lua_release(void) {
    return LUA_RELEASE;
}
