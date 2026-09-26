//! Borrowed immutable UTF-8 ABI: native-endian u64 byte length, then inline bytes.
//! String literals live in their compiled module's data segment. There is no NUL
//! terminator, per-call allocation, or ownership transfer across the native ABI.

/// # Safety
/// `ptr` must address an eight-byte length followed by that many initialized UTF-8
/// bytes in the same live allocation. The entire object must remain immutable and
/// live for the returned borrow; callers may not retain it beyond its code owner.
pub unsafe fn borrow<'a>(ptr: *const u8) -> Result<&'a str, String> {
    if ptr.is_null() {
        return Err("Null String argument".into());
    }
    let len = unsafe { std::ptr::read_unaligned(ptr.cast::<u64>()) };
    if len > (isize::MAX as u64) - 8 {
        return Err("String argument is too large".into());
    }
    let bytes = unsafe { std::slice::from_raw_parts(ptr.add(8), len as usize) };
    std::str::from_utf8(bytes).map_err(|_| "String argument is not UTF-8".into())
}
