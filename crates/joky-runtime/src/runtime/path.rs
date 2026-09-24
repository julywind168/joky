//! Pure native-platform lexical paths. ABI operation tags match Path(u8) MIR.
use super::managed::jk_drop;
use super::string::jk_string_from_utf8;
use std::path::Path;

unsafe fn text<'a>(words: *const usize) -> &'a str {
    let pointer = words.read_unaligned() as *const u8;
    let length = words.add(1).read_unaligned();
    if length == 0 {
        return "";
    }
    // Only generated, typechecked String values enter this internal ABI.
    std::str::from_utf8(std::slice::from_raw_parts(pointer, length)).expect("Joky String UTF-8")
}
fn string(value: &str) -> *mut u8 {
    let pointer = jk_string_from_utf8(value.as_ptr(), value.len());
    if pointer.is_null() {
        std::alloc::handle_alloc_error(std::alloc::Layout::new::<u8>());
    }
    pointer
}
unsafe fn write_string(result: *mut usize, value: &str) {
    result.write_unaligned(string(value) as usize);
    result.add(1).write_unaligned(value.len());
}
unsafe fn write_option(result: *mut usize, value: Option<&str>) {
    std::ptr::write_bytes(result, 0, 3);
    match value {
        Some(value) => write_string(result.add(1), value),
        None => result.write_unaligned(1),
    }
}

/// Borrows its input words; writes owned output fields. Generated code drops
/// input shares afterwards. No filesystem calls or environment reads occur.
pub(crate) unsafe extern "C" fn jk_path_call(op: u8, args: *const usize, result: *mut usize) {
    if op == 9 {
        let mut cursor = super::managed::jk_dup(args.read_unaligned() as *mut u8);
        let mut paths = Vec::new();
        while !cursor.is_null() {
            let mut words = [0u64; 2];
            assert_eq!(super::list::jk_list_head(cursor, words.as_mut_ptr(), 2), 1);
            paths.push(text(words.as_ptr().cast()).to_owned());
            jk_drop(words[0] as *mut u8);
            let next = super::list::jk_list_tail(cursor);
            jk_drop(cursor);
            cursor = next;
        }
        std::ptr::write_bytes(result, 0, 5);
        match std::env::join_paths(paths).map(|value| {
            // UTF-8 inputs and platform separators preserve valid Unicode.
            value.into_string().expect("Unicode search paths")
        }) {
            Ok(value) => write_string(result.add(1), &value),
            Err(error) => {
                result.write_unaligned(1);
                write_string(result.add(3), &error.to_string());
            }
        }
        return;
    }
    let left = text(args);
    let path = Path::new(left);
    match op {
        0 => write_string(
            result,
            path.join(text(args.add(2))).to_str().expect("Unicode path"),
        ),
        1 => result.write_unaligned(usize::from(path.is_absolute())),
        2 => write_option(result, path.parent().and_then(Path::to_str)),
        3 => write_option(result, path.file_name().and_then(std::ffi::OsStr::to_str)),
        4 => write_option(result, path.file_stem().and_then(std::ffi::OsStr::to_str)),
        5 => write_option(result, path.extension().and_then(std::ffi::OsStr::to_str)),
        6 => {
            let extension = text(args.add(2));
            // Reject before PathBuf::set_extension can unwind across the C ABI.
            if extension.chars().any(std::path::is_separator) {
                eprintln!("panic: path extension contains a separator");
                std::process::abort();
            }
            write_string(
                result,
                path.with_extension(extension)
                    .to_str()
                    .expect("Unicode path"),
            );
        }
        7 => write_option(
            result,
            path.strip_prefix(text(args.add(2)))
                .ok()
                .and_then(Path::to_str),
        ),
        8 => {
            let paths: Vec<_> = std::env::split_paths(left).collect();
            let mut tail = std::ptr::null_mut();
            for path in paths.iter().rev() {
                let value = path.to_str().expect("Unicode search path");
                let words = [string(value) as u64, value.len() as u64];
                tail = super::list::jk_list_cons(words.as_ptr(), 2, &1, 1, tail);
                if tail.is_null() {
                    std::alloc::handle_alloc_error(std::alloc::Layout::new::<u8>());
                }
            }
            result.write_unaligned(tail as usize);
        }
        _ => unreachable!("validated path operation"),
    }
}
