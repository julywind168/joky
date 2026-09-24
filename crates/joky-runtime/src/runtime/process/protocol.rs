use super::super::{bytes, list, map, string};
use super::runner::Outcome;
use super::*;

struct Managed(*mut u8);
impl Drop for Managed {
    fn drop(&mut self) {
        jk_drop(self.0);
    }
}
unsafe fn word(a: *const u8, index: usize) -> usize {
    a.add(index * WORD).cast::<usize>().read_unaligned()
}
unsafe fn text(a: *const u8, index: usize) -> Result<String, String> {
    let pointer = word(a, index) as *mut u8;
    let length = word(a, index + 1);
    let header = valid_header(pointer).ok_or("invalid process String")?;
    if header.kind != RuntimeValueKind::String as u8 || header.payload_size != length {
        return Err("invalid process String layout".into());
    }
    std::str::from_utf8(std::slice::from_raw_parts(pointer, length))
        .map(str::to_owned)
        .map_err(|_| "invalid process UTF-8".into())
}
fn valid_collection(pointer: *mut u8, kind: RuntimeValueKind) -> bool {
    pointer.is_null() || unsafe { valid_header(pointer) }.is_some_and(|h| h.kind == kind as u8)
}
unsafe fn strings(pointer: *mut u8) -> Result<Vec<String>, String> {
    if !valid_collection(pointer, RuntimeValueKind::List) {
        return Err("invalid process argument list".into());
    }
    let mut cursor = Managed(super::super::managed::jk_dup(pointer));
    let mut result = Vec::new();
    while !cursor.0.is_null() {
        let mut head = [0u64; 2];
        if list::jk_list_head(cursor.0, head.as_mut_ptr(), 2) == 0 {
            return Err("invalid process argument entry".into());
        }
        let _entry = Managed(head[0] as *mut u8);
        result.push(text(head.as_ptr().cast(), 0)?);
        let tail = list::jk_list_tail(cursor.0);
        cursor = Managed(tail);
    }
    Ok(result)
}
unsafe fn variables(
    pointer: *mut u8,
    replace: bool,
) -> Result<Vec<(String, Option<String>)>, String> {
    if !valid_collection(pointer, RuntimeValueKind::Map) {
        return Err("invalid child environment map".into());
    }
    let mut cursor = Managed(map::jk_map_cursor_new(pointer));
    let mut result = Vec::new();
    while !cursor.0.is_null() {
        let mut key = [0u64; 2];
        let mut value = [0u64; 3];
        // step consumes its input cursor and returns the successor.
        cursor.0 = map::jk_map_cursor_step(
            cursor.0,
            0,
            key.as_mut_ptr(),
            2,
            value.as_mut_ptr(),
            if replace { 2 } else { 3 },
        );
        if cursor.0.is_null() {
            break;
        }
        let _key = Managed(key[0] as *mut u8);
        let present = replace || value[0] as u32 == 0;
        let offset = usize::from(!replace);
        let _value = Managed(if present {
            value[offset] as *mut u8
        } else {
            std::ptr::null_mut()
        });
        let name = text(key.as_ptr().cast(), 0)?;
        let value = if present {
            Some(text(value.as_ptr().cast(), offset)?)
        } else {
            None
        };
        result.push((name, value));
    }
    Ok(result)
}

pub(super) unsafe fn decode(
    a: *const u8,
    capture: bool,
) -> Result<(CommandSpec, CaptureInput), String> {
    let config = word(a, 0) as *mut u8;
    let header = valid_header(config).ok_or("invalid Command")?;
    if header.kind != RuntimeValueKind::Class as u8 || header.payload_size != 9 * WORD {
        return Err("invalid Command layout".into());
    }
    let spec = decode_fields(config)?;
    Ok((spec, decode_input(a, capture)?))
}

// Command payloads and flattened Stage values have the same word offsets;
// enum tags in class memory occupy only the low u32 and may have padding.
unsafe fn decode_fields(config: *const u8) -> Result<CommandSpec, String> {
    let program = text(config, 0)?;
    let args = strings(word(config, 2) as *mut u8)?;
    let cwd = match config.add(3 * WORD).cast::<u32>().read_unaligned() {
        0 => Some(text(config, 4)?),
        1 => None,
        _ => return Err("invalid cwd tag".into()),
    };
    let replace = match config.add(6 * WORD).cast::<u32>().read_unaligned() {
        0 => false,
        1 => true,
        _ => return Err("invalid environment policy".into()),
    };
    let env = variables(
        word(config, if replace { 8 } else { 7 }) as *mut u8,
        replace,
    )?;
    Ok(CommandSpec {
        program,
        args,
        cwd,
        replace,
        env,
    })
}

unsafe fn decode_input(a: *const u8, capture: bool) -> Result<CaptureInput, String> {
    let input = if capture {
        let pointer = word(a, 1) as *mut u8;
        let header = valid_header(pointer).ok_or("invalid process input")?;
        if header.kind != RuntimeValueKind::Bytes as u8 {
            return Err("invalid process input kind".into());
        }
        let value = std::slice::from_raw_parts(
            bytes::jk_bytes_data(pointer),
            bytes::jk_bytes_length(pointer),
        )
        .to_vec();
        Some((value, word(a, 2)))
    } else {
        None
    };
    Ok(input)
}

pub(super) unsafe fn decode_pipeline(
    a: *const u8,
    capture: bool,
) -> Result<(Vec<CommandSpec>, CaptureInput), String> {
    let pipeline = word(a, 0) as *mut u8;
    let header = valid_header(pipeline).ok_or("invalid Pipeline")?;
    if header.kind != RuntimeValueKind::Class as u8 || header.payload_size != WORD {
        return Err("invalid Pipeline layout".into());
    }
    let stages = word(pipeline, 0) as *mut u8;
    if !valid_collection(stages, RuntimeValueKind::List) {
        return Err("invalid pipeline stages".into());
    }
    let mut cursor = Managed(super::super::managed::jk_dup(stages));
    let mut commands = Vec::new();
    while !cursor.0.is_null() {
        let mut words = [0u64; 9];
        if list::jk_list_head(cursor.0, words.as_mut_ptr(), 9) == 0 {
            return Err("invalid pipeline stage layout".into());
        }
        let _roots = [0, 2, 4, 7, 8].map(|index| Managed(words[index] as *mut u8));
        commands.push(decode_fields(words.as_ptr().cast())?);
        cursor = Managed(list::jk_list_tail(cursor.0));
    }
    if commands.is_empty() {
        return Err("process pipeline must contain at least one command".into());
    }
    commands.reverse();
    Ok((commands, decode_input(a, capture)?))
}

fn allocated(pointer: *mut u8) -> *mut u8 {
    if pointer.is_null() {
        std::alloc::handle_alloc_error(std::alloc::Layout::new::<u8>());
    }
    pointer
}
pub(super) fn complete(
    c: &Continuation,
    h: *mut Continuation,
    op: u64,
    capture: bool,
    outcome: Result<Outcome, String>,
) {
    let mut words = [0usize; 8];
    let count = if capture { 8 } else { 6 };
    let mut owned = Vec::new();
    let result = outcome.and_then(|outcome| {
        if let Some(code) = outcome.status.code() {
            words[2] = code as u32 as usize;
        } else {
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                words[1] = 1;
                words[3] = outcome
                    .status
                    .signal()
                    .ok_or("unknown process exit status")? as usize;
            }
            #[cfg(not(unix))]
            {
                return Err("unknown process exit status".to_owned());
            }
        }
        if capture {
            let stdout = allocated(bytes::jk_bytes_from_data(
                outcome.stdout.as_ptr(),
                outcome.stdout.len(),
            ));
            let stderr = allocated(bytes::jk_bytes_from_data(
                outcome.stderr.as_ptr(),
                outcome.stderr.len(),
            ));
            words[4] = stdout as usize;
            words[5] = stderr as usize;
            owned.extend([stdout, stderr]);
        }
        Ok(())
    });
    if let Err(error) = result {
        words.fill(0);
        words[0] = 1;
        let pointer = allocated(string::jk_string_from_utf8(error.as_ptr(), error.len()));
        words[count - 2] = pointer as usize;
        words[count - 1] = error.len();
        owned.push(pointer);
    }
    if !c.complete_suspend_with_payload(h, op, words.as_ptr().cast(), count * WORD) {
        for pointer in owned {
            jk_drop(pointer);
        }
    }
}

pub(super) fn complete_pipeline(
    c: &Continuation,
    h: *mut Continuation,
    op: u64,
    capture: bool,
    outcome: Result<super::runner::PipelineOutcome, String>,
) {
    let mut words = [0usize; 6];
    let count = if capture { 6 } else { 4 };
    let mut owned = Vec::new();
    let result = outcome.and_then(|outcome| {
        let statuses = outcome
            .statuses
            .into_iter()
            .map(status_words)
            .collect::<Result<Vec<_>, _>>()?;
        let mut list = std::ptr::null_mut();
        for status in statuses.iter().rev() {
            list = allocated(super::super::list::jk_list_cons(
                status.as_ptr(),
                3,
                &0,
                1,
                list,
            ));
        }
        words[1] = list as usize;
        owned.push(list);
        if capture {
            let stdout = allocated(bytes::jk_bytes_from_data(
                outcome.stdout.as_ptr(),
                outcome.stdout.len(),
            ));
            words[2] = stdout as usize;
            owned.push(stdout);
            let mut list = std::ptr::null_mut();
            for stderr in outcome.stderrs.iter().rev() {
                let value = allocated(bytes::jk_bytes_from_data(stderr.as_ptr(), stderr.len()));
                list = allocated(super::super::list::jk_list_cons(
                    &(value as u64),
                    1,
                    &1,
                    1,
                    list,
                ));
            }
            words[3] = list as usize;
            owned.push(list);
        }
        Ok(())
    });
    if let Err(error) = result {
        words.fill(0);
        words[0] = 1;
        let pointer = allocated(string::jk_string_from_utf8(error.as_ptr(), error.len()));
        words[count - 2] = pointer as usize;
        words[count - 1] = error.len();
        owned.push(pointer);
    }
    if !c.complete_suspend_with_payload(h, op, words.as_ptr().cast(), count * WORD) {
        for pointer in owned {
            jk_drop(pointer);
        }
    }
}

fn status_words(status: std::process::ExitStatus) -> Result<[u64; 3], String> {
    if let Some(code) = status.code() {
        return Ok([0, code as u32 as u64, 0]);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        Ok([
            1,
            0,
            status.signal().ok_or("unknown process exit status")? as u64,
        ])
    }
    #[cfg(not(unix))]
    Err("unknown process exit status".into())
}
