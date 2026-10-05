//! Synchronous UTF-8 stdout boundary shared by all execution backends.
use crate::{Error, ErrorCode, ErrorPhase};
use std::io::Write;

/// # Safety
/// `text` is a live immutable TextViewV2 descriptor and UTF-8 payload for this call.
#[no_mangle]
pub unsafe extern "C" fn joky_io_write_stdout_v2(text: *const u8) -> i32 {
    let result = std::panic::catch_unwind(|| {
        let text = crate::runtime::string::borrow(text)?;
        std::io::stdout()
            .lock()
            .write_all(text.as_bytes())
            .map_err(|error| {
                Error::new_with_code(ErrorPhase::Runtime, ErrorCode::IoError, error.to_string())
            })
    })
    .unwrap_or_else(|_| {
        Err(Error::new_with_code(
            ErrorPhase::Runtime,
            ErrorCode::Panic,
            "Stdout write panicked",
        ))
    });
    match result {
        Ok(()) => 0,
        Err(error) => {
            crate::runtime::error::record_error(error);
            -1
        }
    }
}
