//! Stable C symbols exported by the runtime static library for AOT objects.

/// Link-time compatibility marker referenced by every generated AOT launcher.
#[no_mangle]
pub extern "C" fn jk_aot_runtime_abi_v30() {}

/// Initialize invocation arguments before the AOT launcher enters its scope.
///
/// # Safety
/// For positive `argc`, `argv` must address that many pointers and every
/// argument after the program name must point to a NUL-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn jk_aot_set_args(argc: i32, argv: *const *const std::ffi::c_char) -> u8 {
    if argc < 0 || (argc != 0 && argv.is_null()) {
        return 0;
    }
    // The Windows C runtime's narrow argv is encoded in the local code page.
    // Read the original wide command line to preserve Unicode and lone surrogates.
    #[cfg(windows)]
    {
        return crate::runtime::scope::set_default_args(std::env::args_os().skip(1).collect())
            as u8;
    }
    #[cfg(not(windows))]
    {
        let mut args = Vec::with_capacity((argc as usize).saturating_sub(1));
        for index in 1..argc as usize {
            let pointer = unsafe { *argv.add(index) };
            if pointer.is_null() {
                return 0;
            }
            let bytes = unsafe { std::ffi::CStr::from_ptr(pointer) }.to_bytes();
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStringExt;
                args.push(std::ffi::OsString::from_vec(bytes.to_vec()));
            }
            #[cfg(not(unix))]
            {
                let Ok(value) = std::str::from_utf8(bytes) else {
                    return 0;
                };
                args.push(std::ffi::OsString::from(value));
            }
        }
        crate::runtime::scope::set_default_args(args) as u8
    }
}

#[no_mangle]
pub extern "C" fn jk_batch_new(input: *mut u8, limit: u64) -> *mut u8 {
    super::batch::jk_batch_new(input, limit)
}

#[no_mangle]
pub unsafe extern "C" fn jk_batch_worker(batch: *mut u8) -> u8 {
    unsafe { super::batch::jk_batch_worker(batch) }
}

#[no_mangle]
pub unsafe extern "C" fn jk_batch_next(batch: *mut u8, index: *mut u64) -> *mut u8 {
    unsafe { super::batch::jk_batch_next(batch, index) }
}

#[no_mangle]
pub unsafe extern "C" fn jk_batch_push(batch: *mut u8, index: u64, result: *mut u8) {
    unsafe { super::batch::jk_batch_push(batch, index, result) }
}

#[no_mangle]
pub unsafe extern "C" fn jk_batch_finish(batch: *mut u8) -> *mut u8 {
    unsafe { super::batch::jk_batch_finish(batch) }
}

#[no_mangle]
pub extern "C" fn jk_seq_new() -> *mut u8 {
    super::list::jk_seq_new()
}

#[no_mangle]
pub unsafe extern "C" fn jk_seq_push(builder: *mut u8, node: *mut u8) {
    unsafe { super::list::jk_seq_push(builder, node) }
}

#[no_mangle]
pub unsafe extern "C" fn jk_seq_finish(builder: *mut u8) -> *mut u8 {
    unsafe { super::list::jk_seq_finish(builder) }
}

#[no_mangle]
pub extern "C" fn jk_hasher_new() -> *mut u8 {
    super::hashing::jk_hasher_new()
}
#[no_mangle]
pub extern "C" fn jk_hasher_word(state: *mut u8, word: u64) {
    super::hashing::jk_hasher_word(state, word)
}
#[no_mangle]
pub extern "C" fn jk_hasher_string(state: *mut u8, text: *const u8, length: usize) {
    super::hashing::jk_hasher_string(state, text, length)
}
#[no_mangle]
pub extern "C" fn jk_hasher_finish(state: *mut u8) -> u64 {
    super::hashing::jk_hasher_finish(state)
}
#[no_mangle]
pub extern "C" fn jk_hasher_bytes(state: *mut u8, bytes: *mut u8) {
    super::hashing::jk_hasher_bytes(state, bytes)
}

#[no_mangle]
pub extern "C" fn jk_mut_map_new(
    kw: usize,
    km: usize,
    vw: usize,
    vm: usize,
    ops: usize,
) -> *mut u8 {
    super::mut_map::jk_mut_map_new(kw, km, vw, vm, ops)
}
#[no_mangle]
pub extern "C" fn jk_mut_map_insert(
    map: *mut u8,
    key: *const u64,
    kc: usize,
    km: *const u64,
    kmc: usize,
    value: *const u64,
    vc: usize,
    vm: *const u64,
    vmc: usize,
    output: *mut u64,
    oc: usize,
) -> u8 {
    super::mut_map::jk_mut_map_insert(map, key, kc, km, kmc, value, vc, vm, vmc, output, oc)
}
#[no_mangle]
pub extern "C" fn jk_mut_map_get(
    map: *mut u8,
    key: *const u64,
    kc: usize,
    ops: usize,
    output: *mut u64,
    oc: usize,
) -> u8 {
    super::mut_map::jk_mut_map_get(map, key, kc, ops, output, oc)
}
#[no_mangle]
pub extern "C" fn jk_mut_map_remove(
    map: *mut u8,
    key: *const u64,
    kc: usize,
    ops: usize,
    output: *mut u64,
    oc: usize,
) -> u8 {
    super::mut_map::jk_mut_map_remove(map, key, kc, ops, output, oc)
}
#[no_mangle]
pub extern "C" fn jk_mut_map_contains_key(
    map: *mut u8,
    key: *const u64,
    kc: usize,
    ops: usize,
) -> u8 {
    super::mut_map::jk_mut_map_contains_key(map, key, kc, ops)
}
#[no_mangle]
pub extern "C" fn jk_mut_map_length(map: *mut u8) -> usize {
    super::mut_map::jk_mut_map_length(map)
}
#[no_mangle]
pub extern "C" fn jk_mut_map_capacity(map: *mut u8) -> usize {
    super::mut_map::jk_mut_map_capacity(map)
}
#[no_mangle]
pub extern "C" fn jk_mut_map_is_empty(map: *mut u8) -> u8 {
    super::mut_map::jk_mut_map_is_empty(map)
}

#[no_mangle]
pub unsafe extern "C" fn jk_mut_map_cursor_new(map: *mut u8) -> *mut u8 {
    unsafe { super::mut_map::jk_mut_map_cursor_new(map) }
}

#[no_mangle]
pub unsafe extern "C" fn jk_mut_map_cursor_step(
    object: *mut u8,
    projection: usize,
    key_output: *mut u64,
    key_count: usize,
    value_output: *mut u64,
    value_count: usize,
) -> *mut u8 {
    unsafe {
        super::mut_map::jk_mut_map_cursor_step(
            object,
            projection,
            key_output,
            key_count,
            value_output,
            value_count,
        )
    }
}

#[no_mangle]
pub extern "C" fn jk_mut_map_to_list(object: *mut u8, keys_only: u8) -> *mut u8 {
    super::mut_map::jk_mut_map_to_list(object, keys_only)
}

/// Drain work before retiring addresses in the statically linked program.
#[no_mangle]
pub extern "C" fn jk_aot_shutdown() {
    let scope = super::scope::current_or_default();
    scope.close_and_wait();
    super::continuation::unregister_machine_entries_for_scope(scope.id());
    super::callback::retire_scope(scope.id());
    super::task::reset_root_failure();
}

#[no_mangle]
pub unsafe extern "C" fn jk_callback_new(
    code: usize,
    environment: usize,
    entry: usize,
    invoke: usize,
    fallback: u64,
    result_size: usize,
) -> *mut u8 {
    unsafe {
        super::callback::jk_callback_new(code, environment, entry, invoke, fallback, result_size)
    }
}

#[no_mangle]
pub unsafe extern "C" fn jk_callback_context(payload: *mut u8) -> usize {
    unsafe { super::callback::jk_callback_context(payload) }
}

#[no_mangle]
pub unsafe extern "C" fn jk_callback_function(payload: *mut u8) -> usize {
    unsafe { super::callback::jk_callback_function(payload) }
}

#[no_mangle]
pub unsafe extern "C" fn jk_callback_failed(payload: *mut u8) -> u8 {
    unsafe { super::callback::jk_callback_failed(payload) }
}

#[no_mangle]
pub unsafe extern "C" fn jk_callback_invoke(token: usize, arguments: *const u64, result: *mut u64) {
    unsafe { super::callback::jk_callback_invoke(token, arguments, result) }
}

#[no_mangle]
pub extern "C" fn jk_string_eq(
    left: *mut u8,
    left_length: usize,
    right: *mut u8,
    right_length: usize,
) -> u8 {
    super::string::jk_string_eq(left, left_length, right, right_length)
}

#[no_mangle]
pub extern "C" fn jk_string_compare(
    left: *mut u8,
    left_length: usize,
    right: *mut u8,
    right_length: usize,
) -> i32 {
    super::string::jk_string_compare(left, left_length, right, right_length)
}

#[no_mangle]
pub extern "C" fn jk_println(pointer: *const u8, length: usize) {
    crate::runtime::io::jk_println(pointer, length)
}

#[no_mangle]
pub extern "C" fn jk_print(pointer: *const u8, length: usize) {
    crate::runtime::io::jk_print(pointer, length)
}

#[no_mangle]
pub extern "C" fn jk_panic(pointer: *const u8, length: usize) {
    crate::runtime::io::jk_panic(pointer, length)
}

#[no_mangle]
pub extern "C" fn jk_string_from_utf8(data: *const u8, length: usize) -> *mut u8 {
    crate::runtime::string::jk_string_from_utf8(data, length)
}

#[no_mangle]
pub extern "C" fn jk_string_len(object: *mut u8) -> usize {
    crate::runtime::string::jk_string_len(object)
}

#[no_mangle]
pub extern "C" fn jk_string_concat(
    left: *mut u8,
    left_length: usize,
    right: *mut u8,
    right_length: usize,
) -> *mut u8 {
    crate::runtime::string::jk_string_concat(left, left_length, right, right_length)
}

#[no_mangle]
pub extern "C" fn jk_string_starts_with(
    object: *mut u8,
    length: usize,
    prefix: *mut u8,
    prefix_length: usize,
) -> u8 {
    super::string::jk_string_starts_with(object, length, prefix, prefix_length)
}

#[no_mangle]
pub extern "C" fn jk_string_ends_with(
    object: *mut u8,
    length: usize,
    suffix: *mut u8,
    suffix_length: usize,
) -> u8 {
    super::string::jk_string_ends_with(object, length, suffix, suffix_length)
}

#[no_mangle]
pub extern "C" fn jk_string_contains(
    object: *mut u8,
    length: usize,
    needle: *mut u8,
    needle_length: usize,
) -> u8 {
    super::string::jk_string_contains(object, length, needle, needle_length)
}

#[no_mangle]
pub extern "C" fn jk_string_scalar_count(object: *mut u8, length: usize) -> usize {
    super::string::jk_string_scalar_count(object, length)
}

#[no_mangle]
pub extern "C" fn jk_string_grapheme_count(object: *mut u8, length: usize) -> usize {
    super::string::jk_string_grapheme_count(object, length)
}

#[no_mangle]
pub extern "C" fn jk_string_is_ascii(object: *mut u8, length: usize) -> u8 {
    super::string::jk_string_is_ascii(object, length)
}

#[no_mangle]
pub extern "C" fn jk_string_trim(object: *mut u8, length: usize) -> *mut u8 {
    super::string::jk_string_trim(object, length)
}

#[no_mangle]
pub extern "C" fn jk_string_to_upper(object: *mut u8, length: usize) -> *mut u8 {
    super::string::jk_string_to_upper(object, length)
}

#[no_mangle]
pub extern "C" fn jk_string_to_lower(object: *mut u8, length: usize) -> *mut u8 {
    super::string::jk_string_to_lower(object, length)
}

#[no_mangle]
pub extern "C" fn jk_string_split(
    object: *mut u8,
    length: usize,
    separator: *mut u8,
    separator_length: usize,
) -> *mut u8 {
    super::string::jk_string_split(object, length, separator, separator_length)
}

#[no_mangle]
pub extern "C" fn jk_string_replace(
    object: *mut u8,
    length: usize,
    from: *mut u8,
    from_length: usize,
    to: *mut u8,
    to_length: usize,
) -> *mut u8 {
    super::string::jk_string_replace(object, length, from, from_length, to, to_length)
}

#[no_mangle]
pub extern "C" fn jk_string_get_byte(
    object: *mut u8,
    length: usize,
    index: usize,
    output: *mut u8,
) -> u8 {
    super::string::jk_string_get_byte(object, length, index, output)
}

#[no_mangle]
pub extern "C" fn jk_string_slice(
    object: *mut u8,
    length: usize,
    start: usize,
    end: usize,
) -> *mut u8 {
    super::string::jk_string_slice(object, length, start, end)
}

macro_rules! string_parse_exports {
    ($($name:ident: $ty:ty),* $(,)?) => {
        $(#[no_mangle]
        pub extern "C" fn $name(object: *mut u8, length: usize, output: *mut $ty) -> u8 {
            super::string::$name(object, length, output)
        })*
    };
}

string_parse_exports! {
    jk_string_parse_i8: i8,
    jk_string_parse_i16: i16,
    jk_string_parse_i32: i32,
    jk_string_parse_i64: i64,
    jk_string_parse_u8: u8,
    jk_string_parse_u16: u16,
    jk_string_parse_u32: u32,
    jk_string_parse_u64: u64,
    jk_string_parse_f32: f32,
    jk_string_parse_f64: f64,
    jk_string_parse_bool: u8,
}

#[no_mangle]
pub extern "C" fn jk_alloc_object(kind: u8, size: usize, align: usize) -> *mut u8 {
    crate::runtime::managed::jk_alloc_object(kind, size, align)
}

#[no_mangle]
pub extern "C" fn jk_alloc_closure_environment(
    size: usize,
    align: usize,
    drop_callback: Option<unsafe extern "C" fn(*mut u8)>,
) -> *mut u8 {
    crate::runtime::managed::jk_alloc_closure_environment(size, align, drop_callback)
}

#[no_mangle]
pub extern "C" fn jk_cown_new(
    payload: *mut u8,
    payload_drop: Option<unsafe extern "C" fn(*mut u8)>,
) -> *mut u8 {
    crate::runtime::managed::jk_cown_new(payload, payload_drop)
}

#[no_mangle]
pub extern "C" fn jk_cown_acquire_many(
    cowns: *const *mut u8,
    count: usize,
    payloads: *mut *mut u8,
) -> u8 {
    crate::runtime::managed::jk_cown_acquire_many(cowns, count, payloads)
}

#[no_mangle]
pub extern "C" fn jk_cown_payload(cown: *mut u8) -> *mut u8 {
    crate::runtime::managed::jk_cown_payload(cown)
}

#[no_mangle]
pub extern "C" fn jk_cown_release(cown: *mut u8) {
    crate::runtime::managed::jk_cown_release(cown)
}

#[no_mangle]
pub extern "C" fn jk_user_drop_enter() {
    joky_runtime_core::destruction::jk_user_drop_enter()
}

#[no_mangle]
pub extern "C" fn jk_user_drop_exit() {
    joky_runtime_core::destruction::jk_user_drop_exit()
}

#[no_mangle]
pub extern "C" fn jk_show_i64(value: i64) -> *mut u8 {
    crate::runtime::string::jk_show_i64(value)
}

#[no_mangle]
pub extern "C" fn jk_debug_string(data: *const u8, length: usize) -> *mut u8 {
    super::string::jk_debug_string(data, length)
}

#[no_mangle]
pub extern "C" fn jk_debug_bytes(value: *mut u8) -> *mut u8 {
    super::debug::jk_debug_bytes(value)
}

#[no_mangle]
pub extern "C" fn jk_debug_duration(millis: u64) -> *mut u8 {
    super::debug::jk_debug_duration(millis)
}

#[no_mangle]
pub extern "C" fn jk_debug_path(parent: *mut u8, object: *const u8) -> *mut u8 {
    super::debug::jk_debug_path(parent, object)
}

#[no_mangle]
pub extern "C" fn jk_debug_path_status(path: *mut u8) -> i32 {
    super::debug::jk_debug_path_status(path)
}

#[no_mangle]
pub extern "C" fn jk_debug_native_id(value: *mut u8) -> u64 {
    super::debug::jk_debug_native_id(value)
}

#[no_mangle]
pub extern "C" fn jk_bytes_new() -> *mut u8 {
    super::bytes::jk_bytes_new()
}

#[no_mangle]
pub extern "C" fn jk_bytes_is_empty(object: *mut u8) -> u8 {
    super::bytes::jk_bytes_is_empty(object)
}

#[no_mangle]
pub extern "C" fn jk_bytes_get(object: *mut u8, index: usize, output: *mut u8) -> u8 {
    super::bytes::jk_bytes_get(object, index, output)
}

#[no_mangle]
pub extern "C" fn jk_bytes_slice(object: *mut u8, start: usize, length: usize) -> *mut u8 {
    super::bytes::jk_bytes_slice(object, start, length)
}

#[no_mangle]
pub extern "C" fn jk_bytes_concat(left: *mut u8, right: *mut u8) -> *mut u8 {
    super::bytes::jk_bytes_concat(left, right)
}

#[no_mangle]
pub unsafe extern "C" fn jk_bytes_cursor_new(bytes: *mut u8) -> *mut u8 {
    super::bytes::jk_bytes_cursor_new(bytes)
}

#[no_mangle]
pub unsafe extern "C" fn jk_bytes_cursor_step(object: *mut u8, output: *mut u8) -> *mut u8 {
    super::bytes::jk_bytes_cursor_step(object, output)
}

#[no_mangle]
pub extern "C" fn jk_bytes_to_string(object: *mut u8) -> *mut u8 {
    super::bytes::jk_bytes_to_string(object)
}

#[no_mangle]
pub extern "C" fn jk_mut_bytes_with_capacity(capacity: usize) -> *mut u8 {
    super::bytes::jk_mut_bytes_with_capacity(capacity)
}

#[no_mangle]
pub extern "C" fn jk_mut_bytes_new() -> *mut u8 {
    super::bytes::jk_mut_bytes_new()
}

#[no_mangle]
pub extern "C" fn jk_mut_bytes_from_bytes(bytes: *mut u8) -> *mut u8 {
    super::bytes::jk_mut_bytes_from_bytes(bytes)
}

#[no_mangle]
pub extern "C" fn jk_mut_bytes_length(object: *mut u8) -> usize {
    super::bytes::jk_mut_bytes_length(object)
}

#[no_mangle]
pub extern "C" fn jk_mut_bytes_capacity(object: *mut u8) -> usize {
    super::bytes::jk_mut_bytes_capacity(object)
}

#[no_mangle]
pub extern "C" fn jk_mut_bytes_reserve(object: *mut u8, additional: usize) -> u8 {
    super::bytes::jk_mut_bytes_reserve(object, additional)
}

#[no_mangle]
pub extern "C" fn jk_mut_bytes_push(object: *mut u8, value: u8) -> u8 {
    super::bytes::jk_mut_bytes_push(object, value)
}

#[no_mangle]
pub extern "C" fn jk_mut_bytes_get(object: *mut u8, index: usize, output: *mut u8) -> u8 {
    super::bytes::jk_mut_bytes_get(object, index, output)
}

#[no_mangle]
pub extern "C" fn jk_mut_bytes_set(object: *mut u8, index: usize, value: u8) -> u8 {
    super::bytes::jk_mut_bytes_set(object, index, value)
}

#[no_mangle]
pub extern "C" fn jk_mut_bytes_pop(object: *mut u8, output: *mut u8) -> u8 {
    super::bytes::jk_mut_bytes_pop(object, output)
}

#[no_mangle]
pub extern "C" fn jk_mut_bytes_clear(object: *mut u8) -> u8 {
    super::bytes::jk_mut_bytes_clear(object)
}

#[no_mangle]
pub extern "C" fn jk_mut_bytes_extend(object: *mut u8, bytes: *mut u8) -> u8 {
    super::bytes::jk_mut_bytes_extend(object, bytes)
}

#[no_mangle]
pub extern "C" fn jk_mut_bytes_to_bytes(object: *mut u8) -> *mut u8 {
    super::bytes::jk_mut_bytes_to_bytes(object)
}

#[no_mangle]
pub extern "C" fn jk_echo(
    location: *const u8,
    location_length: usize,
    value: *const u8,
    value_length: usize,
) {
    super::io::jk_echo(location, location_length, value, value_length)
}

#[no_mangle]
pub extern "C" fn jk_show_u64(value: u64) -> *mut u8 {
    crate::runtime::string::jk_show_u64(value)
}

#[no_mangle]
pub extern "C" fn jk_show_f64(value: f64) -> *mut u8 {
    crate::runtime::string::jk_show_f64(value)
}

#[no_mangle]
pub extern "C" fn jk_show_bool(value: u8) -> *mut u8 {
    crate::runtime::string::jk_show_bool(value)
}

#[no_mangle]
pub extern "C" fn jk_c_alloc(size: usize) -> *mut u8 {
    joky_runtime_core::c_memory::jk_c_alloc(size)
}

#[no_mangle]
pub extern "C" fn jk_c_free(pointer: *mut u8) {
    joky_runtime_core::c_memory::jk_c_free(pointer)
}

#[no_mangle]
pub extern "C" fn jk_drop(object: *mut u8) {
    crate::runtime::managed::jk_drop(object)
}

#[no_mangle]
pub extern "C" fn jk_dup(object: *mut u8) -> *mut u8 {
    crate::runtime::managed::jk_dup(object)
}

#[no_mangle]
pub unsafe extern "C" fn jk_main_result(tag: u32, pointer: *mut u8, length: usize) {
    unsafe { crate::runtime::scope::jk_main_result(tag, pointer, length) }
}

/// AOT entry points store their error in the current runtime scope.
#[no_mangle]
pub extern "C" fn jk_aot_main_status() -> i32 {
    match crate::runtime::scope::current_or_default().take_main_error() {
        Some(message) => {
            eprintln!("error: {message}");
            1
        }
        None => 0,
    }
}

#[no_mangle]
pub extern "C" fn jk_aot_register_machine_entry(key: usize, entry: *mut std::ffi::c_void) -> u8 {
    crate::runtime::continuation::register_machine_entry_for_scope(
        crate::runtime::scope::current_id(),
        key,
        entry,
    )
}

#[no_mangle]
pub extern "C" fn jk_aot_run_pending_main(main: extern "C" fn() -> u8) -> i32 {
    use crate::runtime::abi::FunctionCallStatus;

    let scope = crate::runtime::scope::current_or_default();
    crate::runtime::task::reset_root_failure();
    let status = crate::runtime::continuation::with_function_pending_boundary(|| match main() {
        0 => FunctionCallStatus::Ready,
        1 => FunctionCallStatus::Pending,
        3 => FunctionCallStatus::Cancelled,
        _ => FunctionCallStatus::Failed,
    });
    scope.wait_for_idle();
    let main_status = jk_aot_main_status();
    if main_status != 0 {
        return main_status;
    }
    if matches!(
        status,
        FunctionCallStatus::Failed | FunctionCallStatus::Cancelled
    ) || scope.function_failure() != 0
    {
        eprintln!("error: pending continuation failed ({status:?})");
        return 1;
    }
    if let Some(operation) = crate::runtime::task::take_root_failure_operation() {
        eprintln!("error: unhandled effect operation {operation}");
        return 1;
    }
    0
}

#[no_mangle]
pub extern "C" fn jk_bytes_eq(left: *mut u8, right: *mut u8) -> u8 {
    super::bytes::jk_bytes_eq(left, right)
}

#[no_mangle]
pub extern "C" fn jk_bytes_compare(left: *mut u8, right: *mut u8) -> i32 {
    super::bytes::jk_bytes_compare(left, right)
}

#[no_mangle]
pub extern "C" fn jk_mut_list_new(word_count: usize, mask_count: usize) -> *mut u8 {
    super::mut_list::jk_mut_list_new(word_count, mask_count)
}

#[no_mangle]
pub extern "C" fn jk_mut_list_length(object: *mut u8) -> usize {
    super::mut_list::jk_mut_list_length(object)
}

#[no_mangle]
pub extern "C" fn jk_mut_list_capacity(object: *mut u8) -> usize {
    super::mut_list::jk_mut_list_capacity(object)
}

#[no_mangle]
pub extern "C" fn jk_mut_list_push(
    object: *mut u8,
    words: *const u64,
    word_count: usize,
    masks: *const u64,
    mask_count: usize,
) -> u8 {
    super::mut_list::jk_mut_list_push(object, words, word_count, masks, mask_count)
}

#[no_mangle]
pub extern "C" fn jk_mut_list_get(
    object: *mut u8,
    index: usize,
    output: *mut u64,
    count: usize,
) -> u8 {
    super::mut_list::jk_mut_list_get(object, index, output, count)
}

#[no_mangle]
pub extern "C" fn jk_mut_list_set(
    object: *mut u8,
    index: usize,
    words: *const u64,
    word_count: usize,
    masks: *const u64,
    mask_count: usize,
) -> u8 {
    super::mut_list::jk_mut_list_set(object, index, words, word_count, masks, mask_count)
}

#[no_mangle]
pub extern "C" fn jk_mut_list_pop(object: *mut u8, output: *mut u64, count: usize) -> u8 {
    super::mut_list::jk_mut_list_pop(object, output, count)
}

#[no_mangle]
pub unsafe extern "C" fn jk_mut_list_cursor_new(list: *mut u8) -> *mut u8 {
    unsafe { super::mut_list::jk_mut_list_cursor_new(list) }
}

#[no_mangle]
pub unsafe extern "C" fn jk_mut_list_cursor_step(
    object: *mut u8,
    output: *mut u64,
    output_count: usize,
) -> *mut u8 {
    unsafe { super::mut_list::jk_mut_list_cursor_step(object, output, output_count) }
}

#[no_mangle]
pub extern "C" fn jk_mut_list_to_list(object: *mut u8) -> *mut u8 {
    super::mut_list::jk_mut_list_to_list(object)
}

#[no_mangle]
pub extern "C" fn jk_bytes_from_data(data: *const u8, length: usize) -> *mut u8 {
    super::bytes::jk_bytes_from_data(data, length)
}

#[no_mangle]
pub extern "C" fn jk_bytes_from_string(object: *mut u8, length: usize) -> *mut u8 {
    super::bytes::jk_bytes_from_string(object, length)
}

#[no_mangle]
pub extern "C" fn jk_bytes_length(object: *mut u8) -> usize {
    super::bytes::jk_bytes_length(object)
}

#[no_mangle]
pub extern "C" fn jk_map_insert(
    map: *mut u8,
    words: *const u64,
    word_count: usize,
    masks: *const u64,
    masks_count: usize,
    key_word_count: usize,
    key_kind: usize,
) -> *mut u8 {
    super::map::jk_map_insert(
        map,
        words,
        word_count,
        masks,
        masks_count,
        key_word_count,
        key_kind,
    )
}

#[no_mangle]
pub extern "C" fn jk_map_get(
    map: *mut u8,
    key: *const u64,
    key_word_count: usize,
    key_kind: usize,
    output: *mut u64,
    output_count: usize,
) -> u8 {
    super::map::jk_map_get(map, key, key_word_count, key_kind, output, output_count)
}

#[no_mangle]
pub extern "C" fn jk_map_remove(
    map: *mut u8,
    key: *const u64,
    key_word_count: usize,
    key_kind: usize,
) -> *mut u8 {
    super::map::jk_map_remove(map, key, key_word_count, key_kind)
}

#[no_mangle]
pub extern "C" fn jk_map_contains_key(
    map: *mut u8,
    key: *const u64,
    key_word_count: usize,
    key_kind: usize,
) -> u8 {
    super::map::jk_map_contains_key(map, key, key_word_count, key_kind)
}

#[no_mangle]
pub extern "C" fn jk_map_length(map: *mut u8) -> usize {
    super::map::jk_map_length(map)
}

#[no_mangle]
pub unsafe extern "C" fn jk_map_cursor_new(map: *mut u8) -> *mut u8 {
    super::map::jk_map_cursor_new(map)
}

#[no_mangle]
pub unsafe extern "C" fn jk_map_cursor_step(
    object: *mut u8,
    projection: usize,
    key_output: *mut u64,
    key_count: usize,
    value_output: *mut u64,
    value_count: usize,
) -> *mut u8 {
    super::map::jk_map_cursor_step(
        object,
        projection,
        key_output,
        key_count,
        value_output,
        value_count,
    )
}

#[no_mangle]
pub extern "C" fn jk_list_cons(
    words: *const u64,
    word_count: usize,
    masks: *const u64,
    masks_count: usize,
    tail: *mut u8,
) -> *mut u8 {
    super::list::jk_list_cons(words, word_count, masks, masks_count, tail)
}

#[no_mangle]
pub extern "C" fn jk_list_head(object: *mut u8, output: *mut u64, output_count: usize) -> u8 {
    super::list::jk_list_head(object, output, output_count)
}

#[no_mangle]
pub extern "C" fn jk_list_tail(object: *mut u8) -> *mut u8 {
    super::list::jk_list_tail(object)
}

#[no_mangle]
pub extern "C" fn jk_list_length(object: *mut u8) -> usize {
    super::list::jk_list_length(object)
}

#[no_mangle]
pub extern "C" fn jk_list_reverse(object: *mut u8) -> *mut u8 {
    super::list::jk_list_reverse(object)
}

/// Execute a pure lexical path operation through the generated-code ABI.
///
/// # Safety
/// `op` must be a valid path operation tag; `arguments` and `result` must
/// address the corresponding flattened input and output layouts. Inputs
/// remain borrowed; the caller takes ownership of managed result fields.
#[no_mangle]
pub unsafe extern "C" fn jk_path_call(op: u8, arguments: *const usize, result: *mut usize) {
    unsafe { super::path::jk_path_call(op, arguments, result) }
}

#[cfg(test)]
mod tests {
    #[test]
    fn aot_machine_registration_rejects_conflicts_and_isolates_scopes() {
        use crate::runtime::continuation::registries::registered_machine_entry;
        use std::ffi::c_void;

        extern "C" fn first_entry(_: *mut c_void) {}
        extern "C" fn second_entry(handle: *mut c_void) {
            std::hint::black_box(handle);
        }
        let first = first_entry as *const () as *mut c_void;
        let second = second_entry as *const () as *mut c_void;
        let scope = crate::host::RuntimeScope::new();
        let other = crate::host::RuntimeScope::new();
        let _guard = scope.enter();
        let scope_id = crate::runtime::scope::current_id();
        assert_eq!(super::jk_aot_register_machine_entry(0, first), 0);
        assert_eq!(
            super::jk_aot_register_machine_entry(1, std::ptr::null_mut()),
            0
        );
        assert_eq!(registered_machine_entry(scope_id, 0), None);
        assert_eq!(registered_machine_entry(scope_id, 1), None);
        assert_eq!(super::jk_aot_register_machine_entry(1, first), 1);
        assert_eq!(super::jk_aot_register_machine_entry(1, first), 1);
        assert_eq!(super::jk_aot_register_machine_entry(1, second), 0);
        assert_eq!(registered_machine_entry(scope_id, 1), Some(first));
        {
            let _guard = other.enter();
            let other_id = crate::runtime::scope::current_id();
            assert_eq!(super::jk_aot_register_machine_entry(1, second), 1);
            assert_eq!(registered_machine_entry(other_id, 1), Some(second));
            // The AOT shutdown path must retire only the entered scope.
            {
                let _guard = scope.enter();
                super::jk_aot_shutdown();
                super::jk_aot_shutdown();
            }
            assert_eq!(registered_machine_entry(scope_id, 1), None);
            assert_eq!(registered_machine_entry(other_id, 1), Some(second));
            other.close_and_wait();
            assert_eq!(registered_machine_entry(other_id, 1), None);
        }
    }

    #[test]
    fn exported_aot_marker_matches_the_shared_abi_contract() {
        assert_eq!(joky_runtime_abi::AOT_RUNTIME_ABI_VERSION, 30);
        assert_eq!(
            joky_runtime_abi::AOT_RUNTIME_ABI_SYMBOL,
            "jk_aot_runtime_abi_v30"
        );
        super::jk_aot_runtime_abi_v30();
    }
}
