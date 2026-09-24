//! The continuation owns input handles. Jobs retain native resources and shared
//! strings until completion; rejected results keep their producer-side owner.

use super::*;
use crate::runtime::blocking;
use crate::runtime::managed::{jk_drop, jk_dup};
use crate::runtime::string::{jk_string_from_utf8, jk_string_len};

const WORD: usize = std::mem::size_of::<usize>();

struct Managed(usize);

impl Drop for Managed {
    fn drop(&mut self) {
        jk_drop(self.0 as *mut u8);
    }
}

struct Text {
    value: Managed,
    length: usize,
}

impl Text {
    unsafe fn retain(arguments: *const u8, offset: usize) -> Option<Self> {
        let pointer = words(arguments, offset) as *mut u8;
        let length = words(arguments, offset + 1);
        if pointer.is_null() || jk_string_len(pointer) != length {
            return None;
        }
        let retained = jk_dup(pointer);
        (!retained.is_null()).then_some(Self {
            value: Managed(retained as usize),
            length,
        })
    }

    fn as_str(&self) -> Result<&str, String> {
        let bytes = unsafe { std::slice::from_raw_parts(self.value.0 as *const u8, self.length) };
        std::str::from_utf8(bytes).map_err(|_| "invalid UTF-8".to_owned())
    }
}

fn words(pointer: *const u8, index: usize) -> usize {
    unsafe { std::ptr::read_unaligned(pointer.add(index * WORD).cast()) }
}

#[derive(Clone, Copy)]
enum Operation {
    Open,
    Prepare,
    Execute,
    BindText,
    BindI64,
    BindNull,
    BindF64,
    BindBlob,
    Query,
    Step,
    Finalize,
    Close,
}

impl Operation {
    fn argument_words(self) -> usize {
        match self {
            Self::Open | Self::BindNull => 2,
            Self::Prepare | Self::BindI64 | Self::BindF64 | Self::BindBlob => 3,
            Self::BindText => 4,
            _ => 1,
        }
    }

    fn result_words(self) -> usize {
        match self {
            Self::Query => 5,
            Self::Step => 6,
            Self::Finalize | Self::Close => 3,
            _ => 4,
        }
    }
}

macro_rules! start {
    ($name:ident, $kind:ident) => {
        pub(super) unsafe extern "C" fn $name(
            handle: *mut Continuation,
            operation: u64,
            arguments: *const u8,
            size: usize,
            _: *mut u8,
            result_size: usize,
        ) -> u8 {
            start_operation(
                handle,
                operation,
                arguments,
                size,
                result_size,
                Operation::$kind,
            )
        }
    };
}

start!(sqlite_open_start, Open);
start!(sqlite_prepare_start, Prepare);
start!(sqlite_statement_execute_start, Execute);
start!(sqlite_bind_text_start, BindText);
start!(sqlite_bind_i64_start, BindI64);
start!(sqlite_finalize_start, Finalize);
start!(sqlite_close_start, Close);
start!(sqlite_bind_null_start, BindNull);
start!(sqlite_bind_f64_start, BindF64);
start!(sqlite_bind_blob_start, BindBlob);
start!(sqlite_query_start, Query);
start!(sqlite_step_start, Step);

enum Request {
    Open(Text),
    Prepare(Arc<SqliteConnection>, Text),
    Statement(Arc<SqliteStatement>, Operation, u64, Option<Text>, i64),
    BindBlob(Arc<SqliteStatement>, u64, Managed),
    Close(Arc<SqliteConnection>),
}

struct QueuedRequest {
    // On queued cancellation, native resources enqueue cleanup before the
    // scope lease is released. Field drop order is part of this handoff.
    request: Option<Request>,
    _work: crate::runtime::scope::ScopeWork,
}

impl QueuedRequest {
    fn run(
        mut self,
        pending: &Continuation,
        handle: *mut Continuation,
        operation: u64,
        result_words: usize,
    ) {
        let _scope_guard = pending.enter_scope();
        if pending.is_cancelled() {
            return;
        }
        let result = self
            .request
            .take()
            .expect("queued SQLite request")
            .execute(pending.scope_id());
        complete(pending, handle, operation, result_words, result);
    }
}

unsafe fn request(
    arguments: *const u8,
    kind: Operation,
    scope: ScopeId,
) -> Result<Request, String> {
    let text =
        |offset| Text::retain(arguments, offset).ok_or_else(|| "invalid SQLite string".to_owned());
    match kind {
        Operation::Open => Ok(Request::Open(text(0)?)),
        Operation::Prepare | Operation::Close => {
            let db = connection_from_handle(words(arguments, 0) as *mut u8, scope)
                .ok_or("invalid SQLite connection")?;
            match kind {
                Operation::Prepare => Ok(Request::Prepare(db, text(1)?)),
                _ => Ok(Request::Close(db)),
            }
        }
        _ => {
            let statement = statement_from_handle(words(arguments, 0) as *mut u8, scope)
                .ok_or("invalid SQLite statement")?;
            if matches!(kind, Operation::BindBlob) {
                let pointer = words(arguments, 2) as *mut u8;
                if crate::runtime::bytes::jk_bytes_data(pointer).is_null() {
                    return Err("invalid SQLite blob".to_owned());
                }
                return Ok(Request::BindBlob(
                    statement,
                    words(arguments, 1) as u64,
                    Managed(jk_dup(pointer) as usize),
                ));
            }
            let (index, value, number) = match kind {
                Operation::BindText => (words(arguments, 1) as u64, Some(text(2)?), 0),
                Operation::BindI64 | Operation::BindF64 => {
                    (words(arguments, 1) as u64, None, words(arguments, 2) as i64)
                }
                Operation::BindNull => (words(arguments, 1) as u64, None, 0),
                _ => (0, None, 0),
            };
            Ok(Request::Statement(statement, kind, index, value, number))
        }
    }
}

unsafe fn start_operation(
    handle: *mut Continuation,
    operation: u64,
    arguments: *const u8,
    size: usize,
    result_size: usize,
    kind: Operation,
) -> u8 {
    if arguments.is_null()
        || size != kind.argument_words() * WORD
        || result_size != kind.result_words() * WORD
    {
        return 0;
    }
    let Some(continuation) = Continuation::retain_registered(handle) else {
        return 0;
    };
    let request = match request(arguments, kind, continuation.scope_id()) {
        Ok(request) => request,
        Err(message) => {
            complete(
                &continuation,
                handle,
                operation,
                kind.result_words(),
                Err(message),
            );
            return 1;
        }
    };
    let pending = continuation.clone();
    let address = handle as usize;
    let request = QueuedRequest {
        request: Some(request),
        _work: crate::runtime::scope::current_or_default().begin_cleanup(),
    };
    blocking::enqueue(
        (address, operation),
        || continuation.is_cancelled(),
        move || {
            request.run(
                &pending,
                address as *mut Continuation,
                operation,
                kind.result_words(),
            );
        },
    );
    1
}

enum Output {
    Handle(Managed),
    Rows(Managed),
    Row(Option<(Managed, Managed)>),
    Count(u64),
    Unit,
}

impl Output {
    fn handle(pointer: *mut u8) -> Result<Self, String> {
        if pointer.is_null() {
            Err("could not allocate SQLite handle".to_owned())
        } else {
            Ok(Self::Handle(Managed(pointer as usize)))
        }
    }
}

impl Request {
    fn execute(self, scope: ScopeId) -> Result<Output, String> {
        match self {
            Self::Open(path) => Output::handle(allocate_handle(
                SqliteConnection::open(path.as_str()?)?,
                scope,
            )),
            Self::Prepare(db, sql) => {
                Output::handle(allocate_statement_handle(db.prepare(sql.as_str()?)?, scope))
            }
            Self::Close(db) => {
                // Statements retain their connection until their own cleanup.
                drop(db);
                Ok(Output::Unit)
            }
            Self::BindBlob(statement, index, data) => {
                let pointer = data.0 as *mut u8;
                let bytes = unsafe {
                    std::slice::from_raw_parts(
                        crate::runtime::bytes::jk_bytes_data(pointer),
                        crate::runtime::bytes::jk_bytes_length(pointer),
                    )
                };
                statement.bind_blob(index, bytes)?;
                Output::handle(allocate_statement_handle(statement, scope))
            }
            Self::Statement(statement, kind, index, text, number) => {
                match kind {
                    Operation::BindText => {
                        statement.bind_text(index, text.as_ref().expect("bind text").as_str()?)?
                    }
                    Operation::BindI64 => statement.bind_i64(index, number)?,
                    Operation::BindNull => statement.bind_null(index)?,
                    Operation::BindF64 => {
                        statement.bind_f64(index, f64::from_bits(number as u64))?
                    }
                    Operation::Query => {
                        let Output::Handle(value) =
                            Output::handle(allocate_statement_handle(statement, scope))?
                        else {
                            unreachable!()
                        };
                        return Ok(Output::Rows(value));
                    }
                    Operation::Step => {
                        return match statement.step()? {
                            Some(values) => {
                                let row = row_list(values)?;
                                let Output::Handle(next) =
                                    Output::handle(allocate_statement_handle(statement, scope))?
                                else {
                                    unreachable!()
                                };
                                Ok(Output::Row(Some((row, next))))
                            }
                            None => {
                                statement.finalize()?;
                                Ok(Output::Row(None))
                            }
                        };
                    }
                    Operation::Execute => return statement.execute().map(Output::Count),
                    Operation::Finalize => return statement.finalize().map(|()| Output::Unit),
                    _ => unreachable!("statement operation"),
                }
                Output::handle(allocate_statement_handle(statement, scope))
            }
        }
    }
}

fn row_list(values: Vec<Value>) -> Result<Managed, String> {
    let mut tail = Managed(0);
    for value in values.into_iter().rev() {
        // SqliteValue has a tag and disjoint payloads: i64, f64, String, Bytes.
        // Masks mark only pointers in the active variant, as generated lists do.
        let mut words = [0u64; 6];
        let mut mask = 0u64;
        match value {
            Value::Null => {}
            Value::Integer(value) => {
                words[0] = 1;
                words[1] = value as u64;
            }
            Value::Real(value) => {
                words[0] = 2;
                words[2] = value.to_bits();
            }
            Value::Text(value) => {
                let pointer = jk_string_from_utf8(value.as_ptr(), value.len());
                if pointer.is_null() {
                    return Err("could not allocate SQLite text".to_owned());
                }
                words[0] = 3;
                words[3] = pointer as u64;
                words[4] = value.len() as u64;
                mask = 1 << 3;
            }
            Value::Blob(value) => {
                let pointer =
                    crate::runtime::bytes::jk_bytes_from_data(value.as_ptr(), value.len());
                if pointer.is_null() {
                    return Err("could not allocate SQLite blob".to_owned());
                }
                words[0] = 4;
                words[5] = pointer as u64;
                mask = 1 << 5;
            }
        }
        let next = crate::runtime::list::jk_list_cons(
            words.as_ptr(),
            words.len(),
            &mask,
            1,
            tail.0 as *mut u8,
        );
        // cons consumes its elements and tail even if allocation fails.
        tail.0 = next as usize;
        if next.is_null() {
            return Err("could not allocate SQLite row".to_owned());
        }
    }
    Ok(tail)
}

fn complete(
    continuation: &Continuation,
    handle: *mut Continuation,
    operation: u64,
    result_words: usize,
    result: Result<Output, String>,
) {
    if continuation.is_cancelled() {
        return;
    }
    let mut payload = [0usize; 6];
    let mut owned = [None, None];
    match result {
        Ok(Output::Handle(value)) => {
            payload[1] = value.0;
            owned[0] = Some(value);
        }
        Ok(Output::Rows(value)) => {
            payload[2] = value.0;
            owned[0] = Some(value);
        }
        Ok(Output::Row(Some((row, next)))) => {
            payload[2] = row.0;
            payload[3] = next.0;
            owned = [Some(row), Some(next)];
        }
        Ok(Output::Row(None)) => payload[1] = 1,
        Ok(Output::Count(count)) => payload[1] = count as usize,
        Ok(Output::Unit) => {}
        Err(message) => {
            let pointer = jk_string_from_utf8(message.as_ptr(), message.len());
            payload[0] = 1;
            payload[result_words - 2] = pointer as usize;
            payload[result_words - 1] = message.len();
            owned[0] = Some(Managed(pointer as usize));
        }
    }
    if continuation.complete_suspend_with_payload(
        handle,
        operation,
        payload.as_ptr().cast(),
        result_words * WORD,
    ) {
        // Successful publication transfers the sole managed reference.
        std::mem::forget(owned);
    }
}

pub(super) unsafe extern "C" fn sqlite_cancel(handle: *mut Continuation, operation: u64) -> u8 {
    blocking::cancel((handle as usize, operation));
    1
}

#[cfg(test)]
mod tests;
