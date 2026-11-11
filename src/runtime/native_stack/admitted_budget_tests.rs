//! Exercise the native helper ABIs admitted in stage one on the constrained stack.
use crate::runtime::{
    arena::{self, Arena},
    buffer::Buffer,
    cleanup::{self, NativeFrame},
    error, owned_text,
    place::{self, CopyNode},
    string::JokyText,
};

pub(super) unsafe fn exercise(nodes: &[CopyNode]) {
    let address = |name| {
        arena::symbols()
            .into_iter()
            .chain(cleanup::symbols())
            .chain(place::symbols())
            .find(|(symbol, _)| *symbol == name)
            .unwrap()
            .1
    };
    let __joky_arena_new: unsafe extern "C" fn() -> *mut Arena =
        std::mem::transmute(address("__joky_arena_new"));
    let __joky_arena_drop: unsafe extern "C" fn(*mut Arena) =
        std::mem::transmute(address("__joky_arena_drop"));
    let __joky_cleanup_register: unsafe extern "C" fn(*mut NativeFrame, usize, *mut Buffer) =
        std::mem::transmute(address("__joky_cleanup_register"));
    let __joky_cleanup_append: unsafe extern "C" fn(*mut NativeFrame, usize, usize, *mut Buffer) =
        std::mem::transmute(address("__joky_cleanup_append"));
    let __joky_cleanup_forget: unsafe extern "C" fn(*mut NativeFrame, usize) =
        std::mem::transmute(address("__joky_cleanup_forget"));
    let __joky_cleanup_move: unsafe extern "C" fn(*mut NativeFrame, usize, usize) =
        std::mem::transmute(address("__joky_cleanup_move"));
    let __joky_cleanup_run: unsafe extern "C" fn(*mut NativeFrame, usize, usize) =
        std::mem::transmute(address("__joky_cleanup_run"));
    let __joky_cleanup_storage_v2: unsafe extern "C" fn(*mut NativeFrame, usize, *mut u8, usize) =
        std::mem::transmute(address("__joky_cleanup_storage_v2"));
    let __joky_cleanup_storage_owner: unsafe extern "C" fn(*mut NativeFrame, usize, usize) =
        std::mem::transmute(address("__joky_cleanup_storage_owner"));
    let __joky_place_begin: unsafe extern "C" fn(
        *mut NativeFrame,
        usize,
        usize,
        usize,
        u64,
    ) -> *mut u8 = std::mem::transmute(address("__joky_place_begin"));
    let __joky_place_materialize: unsafe extern "C" fn(
        *mut NativeFrame,
        usize,
        usize,
        usize,
    ) -> *mut u8 = std::mem::transmute(address("__joky_place_materialize"));
    let __joky_place_describe: unsafe extern "C" fn(
        *mut NativeFrame,
        usize,
        *const CopyNode,
        usize,
    ) = std::mem::transmute(address("__joky_place_describe"));
    let __joky_place_next: unsafe extern "C" fn(*mut NativeFrame, *mut u8, usize) -> *mut u8 =
        std::mem::transmute(address("__joky_place_next"));
    let __joky_place_commit: unsafe extern "C" fn(*mut NativeFrame, *mut u8) =
        std::mem::transmute(address("__joky_place_commit"));
    let __joky_place_move: unsafe extern "C" fn(
        *mut NativeFrame,
        usize,
        *mut u8,
        usize,
    ) -> *mut u8 = std::mem::transmute(address("__joky_place_move"));
    let __joky_place_copy: unsafe extern "C" fn(*mut u8, *const u8, *const CopyNode, usize) =
        std::mem::transmute(address("__joky_place_copy"));
    let __joky_place_initialize: unsafe extern "C" fn(
        *mut u8,
        *const u8,
        *const CopyNode,
        usize,
        usize,
    ) = std::mem::transmute(address("__joky_place_initialize"));
    let __joky_place_replace: unsafe extern "C" fn(
        *mut Arena,
        *mut u8,
        *const u8,
        *const CopyNode,
        usize,
    ) = std::mem::transmute(address("__joky_place_replace"));
    let __joky_place_dispose: unsafe extern "C" fn(*mut Arena, *mut u8, *const CopyNode, usize) =
        std::mem::transmute(address("__joky_place_dispose"));
    let __joky_place_end: unsafe extern "C" fn(*mut NativeFrame, usize) =
        std::mem::transmute(address("__joky_place_end"));
    let arena = __joky_arena_new();
    let frame = cleanup::new(arena);
    // The exposed String entry/result path includes UTF-8 validation, a copy
    // into Local storage, and a separate host-owned result allocation.
    let text = owned_text::input(arena, b"text".as_ptr(), 4);
    assert!(!text.is_null());
    let mut view = JokyText::new("");
    owned_text::view(text, &mut view);
    let mut output = owned_text::JokyOwnedTextV2::default();
    owned_text::result_copy(text, &mut output);
    assert_eq!(output.byte_length, 4);
    owned_text::joky_string_result_free_v2(&mut output);
    __joky_cleanup_register(frame, 1, text);
    let second = owned_text::clone(arena, text, 0, 0, 1);
    __joky_cleanup_register(frame, 2, second);
    __joky_cleanup_append(frame, 1, 2, second);
    __joky_cleanup_move(frame, 3, 1);
    __joky_cleanup_run(frame, 3, 2);

    // Addressable aggregate owners use storage descriptors, not handle lists.
    let mut owners = [
        Buffer::copy_bytes(&mut *arena, b"left").unwrap().as_ptr(),
        Buffer::copy_bytes(&mut *arena, b"right").unwrap().as_ptr(),
    ];
    __joky_cleanup_register(frame, 4, owners[0]);
    __joky_cleanup_storage_v2(frame, 4, owners.as_mut_ptr().cast(), 16);
    __joky_cleanup_storage_owner(frame, 4, 0);
    __joky_cleanup_storage_owner(frame, 4, 8);
    __joky_cleanup_run(frame, 4, 1);
    let returned = Buffer::copy_bytes(&mut *arena, b"return").unwrap();
    __joky_cleanup_register(frame, 5, returned.as_ptr());
    __joky_cleanup_forget(frame, 5);
    __joky_cleanup_register(frame, 6, returned.as_ptr());
    cleanup::finish_return(frame, 6);
    Buffer::release(&mut *arena, returned.as_ptr()).unwrap();

    let frame = cleanup::new(arena);
    let source = __joky_place_begin(frame, 1, 16, 8, 2);
    __joky_place_describe(frame, 1, nodes.as_ptr(), nodes.len());
    for bytes in [b"one", b"two"] {
        let next = __joky_place_next(frame, source, 8);
        next.cast::<*mut Buffer>()
            .write(Buffer::copy_bytes(&mut *arena, bytes).unwrap().as_ptr());
        __joky_place_commit(frame, source);
    }
    let source = __joky_place_move(frame, 2, source, 1);
    let destination = __joky_place_materialize(frame, 3, 16, 8);
    __joky_place_describe(frame, 3, nodes.as_ptr(), nodes.len());
    __joky_place_copy(destination, source, nodes.as_ptr(), nodes.len());
    __joky_place_initialize(source, destination, nodes.as_ptr(), nodes.len(), 16);
    // Replace must dispose its previous owner at maximum descriptor depth.
    destination.cast::<*mut Buffer>().write(
        Buffer::copy_bytes(&mut *arena, b"previous")
            .unwrap()
            .as_ptr(),
    );
    __joky_place_replace(arena, destination, source, nodes.as_ptr(), nodes.len());
    __joky_place_dispose(arena, destination, nodes.as_ptr(), nodes.len());
    __joky_place_end(frame, 3);
    __joky_place_end(frame, 2);
    assert_eq!(error::joky_runtime_error_code(), 0);

    let limit = arena::LIMIT.with(|limit| limit.replace(0));
    assert!(__joky_place_begin(frame, 9, 4096, 8, 512).is_null());
    assert_ne!(error::joky_runtime_error_code(), 0);
    error::joky_runtime_error_clear();
    assert!(__joky_place_materialize(frame, 10, 4096, 8).is_null());
    assert_ne!(error::joky_runtime_error_code(), 0);
    error::joky_runtime_error_clear();
    assert!(owned_text::input(arena, b"new".as_ptr(), 3).is_null());
    assert_ne!(error::joky_runtime_error_code(), 0);
    error::joky_runtime_error_clear();
    cleanup::finish(frame);
    __joky_arena_drop(arena);
    arena::LIMIT.with(|value| value.set(limit));
    arena::LIVE.with(|live| assert_eq!(live.get(), 0));
}
