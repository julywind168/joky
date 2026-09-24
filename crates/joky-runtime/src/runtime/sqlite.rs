//! Runtime primitives shared by the standard SQLite provider.

use std::ffi::{CStr, CString};
use std::sync::{Arc, Mutex};

use libloading::Library;

use super::continuation::Continuation;
use super::managed::{jk_alloc_native_handle, valid_header, RuntimeValueKind};
use super::scope::ScopeId;
use crate::runtime::provider::ProviderStart as provider_hook;

const SQLITE_OK: i32 = 0;
const SQLITE_ROW: i32 = 100;
const SQLITE_DONE: i32 = 101;

/// Small owned wrapper around the dynamically loaded SQLite ABI. Keeping the
/// library handle alive alongside symbols makes provider startup independent
/// of the platform's link configuration.
pub(crate) struct SqliteApi {
    _library: Library,
    pub(crate) open: unsafe extern "C" fn(*const i8, *mut *mut u8) -> i32,
    pub(crate) close: unsafe extern "C" fn(*mut u8) -> i32,
    pub(crate) errmsg: unsafe extern "C" fn(*mut u8) -> *const i8,
    pub(crate) changes: unsafe extern "C" fn(*mut u8) -> i64,
    pub(crate) prepare:
        unsafe extern "C" fn(*mut u8, *const i8, i32, *mut *mut u8, *mut *const i8) -> i32,
    pub(crate) bind_text: unsafe extern "C" fn(
        *mut u8,
        i32,
        *const i8,
        i32,
        Option<unsafe extern "C" fn(*mut u8)>,
    ) -> i32,
    pub(crate) bind_i64: unsafe extern "C" fn(*mut u8, i32, i64) -> i32,
    bind_null: unsafe extern "C" fn(*mut u8, i32) -> i32,
    bind_f64: unsafe extern "C" fn(*mut u8, i32, f64) -> i32,
    bind_blob: unsafe extern "C" fn(
        *mut u8,
        i32,
        *const u8,
        i32,
        Option<unsafe extern "C" fn(*mut u8)>,
    ) -> i32,
    pub(crate) step: unsafe extern "C" fn(*mut u8) -> i32,
    pub(crate) finalize: unsafe extern "C" fn(*mut u8) -> i32,
    pub(crate) column_text: unsafe extern "C" fn(*mut u8, i32) -> *const i8,
    pub(crate) column_bytes: unsafe extern "C" fn(*mut u8, i32) -> i32,
    column_count: unsafe extern "C" fn(*mut u8) -> i32,
    column_type: unsafe extern "C" fn(*mut u8, i32) -> i32,
    column_i64: unsafe extern "C" fn(*mut u8, i32) -> i64,
    column_f64: unsafe extern "C" fn(*mut u8, i32) -> f64,
    column_blob: unsafe extern "C" fn(*mut u8, i32) -> *const u8,
}

impl SqliteApi {
    pub(crate) fn load() -> Result<Self, String> {
        let names: &[&str] = if cfg!(target_os = "macos") {
            &["libsqlite3.dylib"]
        } else if cfg!(target_os = "windows") {
            &["sqlite3.dll"]
        } else {
            &["libsqlite3.so.0", "libsqlite3.so"]
        };
        let mut error = String::new();
        for name in names {
            // SAFETY: symbols are retained for exactly as long as `library`.
            let Ok(library) = (unsafe { Library::new(*name) }) else {
                error = format!("could not load SQLite library {name}");
                continue;
            };
            unsafe {
                macro_rules! symbol {
                    ($name:literal, $ty:ty) => {
                        *library
                            .get::<$ty>($name)
                            .map_err(|e| format!("missing SQLite symbol {:?}: {e}", $name))?
                    };
                }
                return Ok(Self {
                    open: symbol!(
                        b"sqlite3_open\0",
                        unsafe extern "C" fn(*const i8, *mut *mut u8) -> i32
                    ),
                    close: symbol!(b"sqlite3_close_v2\0", unsafe extern "C" fn(*mut u8) -> i32),
                    errmsg: symbol!(
                        b"sqlite3_errmsg\0",
                        unsafe extern "C" fn(*mut u8) -> *const i8
                    ),
                    changes: symbol!(b"sqlite3_changes64\0", unsafe extern "C" fn(*mut u8) -> i64),
                    prepare: symbol!(
                        b"sqlite3_prepare_v2\0",
                        unsafe extern "C" fn(
                            *mut u8,
                            *const i8,
                            i32,
                            *mut *mut u8,
                            *mut *const i8,
                        ) -> i32
                    ),
                    bind_text: symbol!(
                        b"sqlite3_bind_text\0",
                        unsafe extern "C" fn(
                            *mut u8,
                            i32,
                            *const i8,
                            i32,
                            Option<unsafe extern "C" fn(*mut u8)>,
                        ) -> i32
                    ),
                    bind_i64: symbol!(
                        b"sqlite3_bind_int64\0",
                        unsafe extern "C" fn(*mut u8, i32, i64) -> i32
                    ),
                    bind_null: symbol!(
                        b"sqlite3_bind_null\0",
                        unsafe extern "C" fn(*mut u8, i32) -> i32
                    ),
                    bind_f64: symbol!(
                        b"sqlite3_bind_double\0",
                        unsafe extern "C" fn(*mut u8, i32, f64) -> i32
                    ),
                    bind_blob: symbol!(
                        b"sqlite3_bind_blob\0",
                        unsafe extern "C" fn(
                            *mut u8,
                            i32,
                            *const u8,
                            i32,
                            Option<unsafe extern "C" fn(*mut u8)>,
                        ) -> i32
                    ),
                    step: symbol!(b"sqlite3_step\0", unsafe extern "C" fn(*mut u8) -> i32),
                    finalize: symbol!(b"sqlite3_finalize\0", unsafe extern "C" fn(*mut u8) -> i32),
                    column_text: symbol!(
                        b"sqlite3_column_text\0",
                        unsafe extern "C" fn(*mut u8, i32) -> *const i8
                    ),
                    column_bytes: symbol!(
                        b"sqlite3_column_bytes\0",
                        unsafe extern "C" fn(*mut u8, i32) -> i32
                    ),
                    column_count: symbol!(
                        b"sqlite3_column_count\0",
                        unsafe extern "C" fn(*mut u8) -> i32
                    ),
                    column_type: symbol!(
                        b"sqlite3_column_type\0",
                        unsafe extern "C" fn(*mut u8, i32) -> i32
                    ),
                    column_i64: symbol!(
                        b"sqlite3_column_int64\0",
                        unsafe extern "C" fn(*mut u8, i32) -> i64
                    ),
                    column_f64: symbol!(
                        b"sqlite3_column_double\0",
                        unsafe extern "C" fn(*mut u8, i32) -> f64
                    ),
                    column_blob: symbol!(
                        b"sqlite3_column_blob\0",
                        unsafe extern "C" fn(*mut u8, i32) -> *const u8
                    ),
                    _library: library,
                });
            }
        }
        Err(error)
    }

    pub(crate) fn is_ok(code: i32) -> bool {
        code == SQLITE_OK
    }
}

/// The connection lock covers complete operations, including result extraction.
pub(crate) struct SqliteConnection {
    api: Arc<SqliteApi>,
    db: Mutex<Option<*mut u8>>,
    scope: Arc<super::scope::RuntimeScope>,
}

pub(crate) struct SqliteStatement {
    api: Arc<SqliteApi>,
    connection: Arc<SqliteConnection>,
    statement: Mutex<Option<*mut u8>>,
    #[cfg(test)]
    finalized: Arc<std::sync::atomic::AtomicUsize>,
    #[cfg(test)]
    executing: std::sync::atomic::AtomicBool,
}

enum Value {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl SqliteStatement {
    fn with_statement<T>(
        &self,
        f: impl FnOnce(*mut u8, *mut u8) -> Result<T, String>,
    ) -> Result<T, String> {
        #[cfg(test)]
        self.executing
            .store(true, std::sync::atomic::Ordering::Release);
        let connection = self.connection.db.lock().expect("sqlite db mutex");
        let db = connection.ok_or("connection closed")?;
        let statement = self.statement.lock().expect("statement mutex");
        let stmt = statement.ok_or("statement finalized")?;
        f(db, stmt)
    }

    pub(crate) fn bind_text(&self, index: u64, value: &str) -> Result<(), String> {
        let index = binding_index(index)?;
        let length =
            i32::try_from(value.len()).map_err(|_| "SQLite text is too large".to_owned())?;
        self.with_statement(|db, stmt| {
            let transient: unsafe extern "C" fn(*mut u8) = unsafe { std::mem::transmute(-1isize) };
            let code = unsafe {
                (self.api.bind_text)(stmt, index, value.as_ptr().cast(), length, Some(transient))
            };
            SqliteApi::is_ok(code)
                .then_some(())
                .ok_or_else(|| self.connection.error(db))
        })
    }

    pub(crate) fn bind_i64(&self, index: u64, value: i64) -> Result<(), String> {
        let index = binding_index(index)?;
        self.with_statement(|db, stmt| {
            let code = unsafe { (self.api.bind_i64)(stmt, index, value) };
            SqliteApi::is_ok(code)
                .then_some(())
                .ok_or_else(|| self.connection.error(db))
        })
    }

    pub(crate) fn execute(&self) -> Result<u64, String> {
        self.with_statement(|db, stmt| {
            let code = unsafe { (self.api.step)(stmt) };
            if code == SQLITE_DONE {
                Ok(unsafe { (self.api.changes)(db) as u64 })
            } else if code == SQLITE_ROW {
                Err("execute returned a row; use a query operation".to_owned())
            } else {
                Err(self.connection.error(db))
            }
        })
    }

    fn bind_null(&self, index: u64) -> Result<(), String> {
        let index = binding_index(index)?;
        self.with_statement(|db, stmt| {
            SqliteApi::is_ok(unsafe { (self.api.bind_null)(stmt, index) })
                .then_some(())
                .ok_or_else(|| self.connection.error(db))
        })
    }

    fn bind_f64(&self, index: u64, value: f64) -> Result<(), String> {
        if value.is_nan() {
            return Err("SQLite cannot preserve NaN; bind NULL explicitly".to_owned());
        }
        let index = binding_index(index)?;
        self.with_statement(|db, stmt| {
            SqliteApi::is_ok(unsafe { (self.api.bind_f64)(stmt, index, value) })
                .then_some(())
                .ok_or_else(|| self.connection.error(db))
        })
    }

    fn bind_blob(&self, index: u64, value: &[u8]) -> Result<(), String> {
        let index = binding_index(index)?;
        let length =
            i32::try_from(value.len()).map_err(|_| "SQLite blob is too large".to_owned())?;
        self.with_statement(|db, stmt| {
            let transient =
                unsafe { std::mem::transmute::<isize, unsafe extern "C" fn(*mut u8)>(-1) };
            SqliteApi::is_ok(unsafe {
                (self.api.bind_blob)(stmt, index, value.as_ptr(), length, Some(transient))
            })
            .then_some(())
            .ok_or_else(|| self.connection.error(db))
        })
    }

    fn step(&self) -> Result<Option<Vec<Value>>, String> {
        self.with_statement(|db, stmt| {
            match unsafe { (self.api.step)(stmt) } {
                SQLITE_DONE => return Ok(None),
                SQLITE_ROW => {}
                _ => return Err(self.connection.error(db)),
            }
            let mut row = Vec::new();
            for column in 0..unsafe { (self.api.column_count)(stmt) } {
                let value = match unsafe { (self.api.column_type)(stmt, column) } {
                    1 => Value::Integer(unsafe { (self.api.column_i64)(stmt, column) }),
                    2 => Value::Real(unsafe { (self.api.column_f64)(stmt, column) }),
                    kind @ (3 | 4) => {
                        let pointer = if kind == 3 {
                            unsafe { (self.api.column_text)(stmt, column).cast::<u8>() }
                        } else {
                            unsafe { (self.api.column_blob)(stmt, column) }
                        };
                        let length = unsafe { (self.api.column_bytes)(stmt, column) } as usize;
                        if pointer.is_null() && (length != 0 || kind == 3) {
                            return Err("SQLite could not read column data".to_owned());
                        }
                        let bytes = if length == 0 {
                            Vec::new()
                        } else {
                            unsafe { std::slice::from_raw_parts(pointer, length) }.to_vec()
                        };
                        if kind == 3 {
                            Value::Text(
                                String::from_utf8(bytes)
                                    .map_err(|_| "query returned invalid UTF-8".to_owned())?,
                            )
                        } else {
                            Value::Blob(bytes)
                        }
                    }
                    5 => Value::Null,
                    _ => return Err("unknown SQLite column type".to_owned()),
                };
                row.push(value);
            }
            Ok(Some(row))
        })
    }

    fn finalize(&self) -> Result<(), String> {
        let db = self.connection.db.lock().expect("sqlite db mutex");
        let statement = self.statement.lock().expect("statement mutex").take();
        if let Some(statement) = statement {
            let code = unsafe { (self.api.finalize)(statement) };
            #[cfg(test)]
            self.finalized
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if !SqliteApi::is_ok(code) {
                return Err(db.map_or_else(
                    || "connection closed".to_owned(),
                    |db| self.connection.error(db),
                ));
            }
        }
        Ok(())
    }
}

fn binding_index(index: u64) -> Result<i32, String> {
    i32::try_from(index)
        .ok()
        .filter(|index| *index > 0)
        .ok_or_else(|| "SQLite parameter index must be between 1 and 2147483647".to_owned())
}

unsafe impl Send for SqliteStatement {}
unsafe impl Sync for SqliteStatement {}

impl SqliteConnection {
    pub(crate) fn prepare(self: &Arc<Self>, sql: &str) -> Result<Arc<SqliteStatement>, String> {
        let guard = self.db.lock().expect("sqlite db mutex");
        let db = guard.ok_or("connection closed")?;
        let sql = CString::new(sql).map_err(|_| "SQL contains NUL".to_owned())?;
        let mut statement = std::ptr::null_mut();
        let mut tail = std::ptr::null();
        let code = unsafe { (self.api.prepare)(db, sql.as_ptr(), -1, &mut statement, &mut tail) };
        if !SqliteApi::is_ok(code) {
            if !statement.is_null() {
                unsafe { (self.api.finalize)(statement) };
            }
            return Err(self.error(db));
        }
        if statement.is_null() {
            return Err("SQL contains no statement".to_owned());
        }
        // Let SQLite parse trailing whitespace/comments, but reject another
        // statement instead of silently executing only the first one.
        let mut extra = std::ptr::null_mut();
        let tail_code =
            unsafe { (self.api.prepare)(db, tail, -1, &mut extra, std::ptr::null_mut()) };
        if !extra.is_null() || !SqliteApi::is_ok(tail_code) {
            unsafe {
                (self.api.finalize)(extra);
                (self.api.finalize)(statement);
            }
            return Err("prepare accepts exactly one SQL statement".to_owned());
        }
        Ok(Arc::new(SqliteStatement {
            api: Arc::clone(&self.api),
            connection: Arc::clone(self),
            statement: Mutex::new(Some(statement)),
            #[cfg(test)]
            finalized: Arc::default(),
            #[cfg(test)]
            executing: std::sync::atomic::AtomicBool::new(false),
        }))
    }
}

impl Drop for SqliteStatement {
    fn drop(&mut self) {
        if let Some(stmt) = self.statement.get_mut().expect("statement mutex").take() {
            let stmt = stmt as usize;
            let connection = Arc::clone(&self.connection);
            #[cfg(test)]
            let finalized = Arc::clone(&self.finalized);
            enqueue_cleanup(Arc::clone(&connection.scope), move || {
                let _guard = connection.db.lock().expect("sqlite db mutex");
                unsafe { (connection.api.finalize)(stmt as *mut u8) };
                #[cfg(test)]
                finalized.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            });
        }
    }
}

/// Name-keyed hook table: the runtime's side of the registration contract.
/// A new sqlite operation lands here and in the standard module's `eff`
/// declaration; no compiler change is involved.
fn operation_hooks() -> &'static [(&'static str, provider_hook)] {
    &[
        ("open", provider::sqlite_open_start),
        ("prepare", provider::sqlite_prepare_start),
        ("execute", provider::sqlite_statement_execute_start),
        ("bind_text", provider::sqlite_bind_text_start),
        ("bind_i64", provider::sqlite_bind_i64_start),
        ("finalize", provider::sqlite_finalize_start),
        ("close", provider::sqlite_close_start),
        ("bind_null", provider::sqlite_bind_null_start),
        ("bind_f64", provider::sqlite_bind_f64_start),
        ("bind_blob", provider::sqlite_bind_blob_start),
        ("query", provider::sqlite_query_start),
        ("step", provider::sqlite_step_start),
    ]
}

pub(crate) fn register_operations(
    entries: &[crate::runtime::provider::ProviderOperationEntry],
) -> Option<crate::runtime::provider::ProviderScope> {
    crate::runtime::provider::register_named(operation_hooks(), provider::sqlite_cancel, entries)
}

mod provider;

const HANDLE_MAGIC: u64 = 0x4a4b_5351_4c49_5445;

#[repr(C)]
struct NativeSqlite {
    magic: u64,
    scope: ScopeId,
    connection: Arc<SqliteConnection>,
}

unsafe extern "C" fn drop_native_sqlite(pointer: *mut u8) {
    if !pointer.is_null() {
        std::ptr::drop_in_place(pointer.cast::<NativeSqlite>());
    }
}

pub(crate) fn allocate_handle(connection: Arc<SqliteConnection>, scope: ScopeId) -> *mut u8 {
    let pointer = jk_alloc_native_handle(
        std::mem::size_of::<NativeSqlite>(),
        std::mem::align_of::<NativeSqlite>(),
        Some(drop_native_sqlite),
    );
    if !pointer.is_null() {
        unsafe {
            pointer.cast::<NativeSqlite>().write(NativeSqlite {
                magic: HANDLE_MAGIC,
                scope,
                connection,
            });
        }
    }
    pointer
}

#[repr(C)]
struct NativeStatement {
    magic: u64,
    scope: ScopeId,
    statement: Arc<SqliteStatement>,
}
unsafe extern "C" fn drop_native_statement(pointer: *mut u8) {
    if !pointer.is_null() {
        std::ptr::drop_in_place(pointer.cast::<NativeStatement>());
    }
}
pub(crate) fn allocate_statement_handle(
    statement: Arc<SqliteStatement>,
    scope: ScopeId,
) -> *mut u8 {
    let pointer = jk_alloc_native_handle(
        std::mem::size_of::<NativeStatement>(),
        std::mem::align_of::<NativeStatement>(),
        Some(drop_native_statement),
    );
    if !pointer.is_null() {
        unsafe {
            pointer.cast::<NativeStatement>().write(NativeStatement {
                magic: 0x4a4b_5354_4d54_0001,
                scope,
                statement,
            });
        }
    }
    pointer
}

pub(crate) unsafe fn connection_from_handle(
    pointer: *mut u8,
    scope: ScopeId,
) -> Option<Arc<SqliteConnection>> {
    let header = valid_header(pointer)?;
    if header.kind != RuntimeValueKind::NativeHandle as u8
        || header.payload_size != std::mem::size_of::<NativeSqlite>()
    {
        return None;
    }
    let value = &*pointer.cast::<NativeSqlite>();
    (value.magic == HANDLE_MAGIC && value.scope == scope).then(|| Arc::clone(&value.connection))
}

pub(crate) unsafe fn statement_from_handle(
    pointer: *mut u8,
    scope: ScopeId,
) -> Option<Arc<SqliteStatement>> {
    let header = valid_header(pointer)?;
    if header.kind != RuntimeValueKind::NativeHandle as u8
        || header.payload_size != std::mem::size_of::<NativeStatement>()
    {
        return None;
    }
    let value = &*pointer.cast::<NativeStatement>();
    (value.magic == 0x4a4b_5354_4d54_0001 && value.scope == scope)
        .then(|| Arc::clone(&value.statement))
}

// Every use of a connection and its statements holds the connection mutex.
unsafe impl Send for SqliteConnection {}
unsafe impl Sync for SqliteConnection {}

impl SqliteConnection {
    pub(crate) fn open(path: &str) -> Result<Arc<Self>, String> {
        let api = Arc::new(SqliteApi::load()?);
        let path = CString::new(path).map_err(|_| "SQLite path contains NUL".to_owned())?;
        let mut db = std::ptr::null_mut();
        let code = unsafe { (api.open)(path.as_ptr(), &mut db) };
        if !SqliteApi::is_ok(code) {
            let message = if db.is_null() {
                "sqlite3_open failed".to_owned()
            } else {
                let message = unsafe { CStr::from_ptr((api.errmsg)(db)) }
                    .to_string_lossy()
                    .into_owned();
                unsafe { (api.close)(db) };
                message
            };
            return Err(message);
        }
        Ok(Arc::new(Self {
            api,
            db: Mutex::new(Some(db)),
            scope: super::scope::current_or_default(),
        }))
    }

    fn error(&self, db: *mut u8) -> String {
        unsafe { CStr::from_ptr((self.api.errmsg)(db)) }
            .to_string_lossy()
            .into_owned()
    }
}

impl Drop for SqliteConnection {
    fn drop(&mut self) {
        if let Some(db) = self.db.get_mut().expect("sqlite db mutex").take() {
            let db = db as usize;
            let api = Arc::clone(&self.api);
            let scope = Arc::clone(&self.scope);
            enqueue_cleanup(scope, move || {
                unsafe { (api.close)(db as *mut u8) };
            });
        }
    }
}

fn enqueue_cleanup(scope: Arc<super::scope::RuntimeScope>, job: impl FnOnce() + Send + 'static) {
    let work = scope.begin_cleanup();
    super::blocking::enqueue_cleanup(move || {
        let _scope_guard = scope.enter();
        job();
        drop(work);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sqlite_connection_executes_and_reads_first_value() {
        let connection = SqliteConnection::open(":memory:").expect("SQLite should load");
        connection
            .prepare("create table t (value text)")
            .unwrap()
            .execute()
            .unwrap();
        connection
            .prepare("insert into t values ('hello')")
            .unwrap()
            .execute()
            .unwrap();
        let row = connection
            .prepare("select value from t")
            .unwrap()
            .step()
            .unwrap()
            .expect("row");
        assert!(matches!(&row[0], Value::Text(value) if value == "hello"));
    }

    #[test]
    fn sqlite_prepared_statement_binds_text_and_reads_value() {
        let connection = SqliteConnection::open(":memory:").unwrap();
        connection
            .prepare("create table t (name text, age integer)")
            .unwrap()
            .execute()
            .unwrap();
        let insert = connection.prepare("insert into t values (?, ?)").unwrap();
        insert.bind_text(1, "Alice").unwrap();
        insert.bind_i64(2, 42).unwrap();
        insert.execute().unwrap();
        let select = connection
            .prepare("select name from t where age = ?")
            .unwrap();
        select.bind_i64(1, 42).unwrap();
        let row = select.step().unwrap().expect("row");
        assert!(matches!(&row[0], Value::Text(value) if value == "Alice"));
    }

    #[test]
    fn sqlite_validates_sql_bindings_and_preserves_text_bytes() {
        let connection = SqliteConnection::open(":memory:").unwrap();
        for sql in ["", " -- empty", "select 1; select 2", "select 1; invalid"] {
            assert!(connection.prepare(sql).is_err(), "{sql}");
        }
        let statement = connection.prepare("select ?; -- comment").unwrap();
        for index in [0, 2, u64::MAX, (1u64 << 32) + 1] {
            assert!(statement.bind_text(index, "text").is_err());
            assert!(statement.bind_i64(index, 42).is_err());
        }
        statement.bind_text(1, "a\0b").unwrap();
        let row = statement.step().unwrap().expect("row");
        assert!(matches!(&row[0], Value::Text(value) if value == "a\0b"));
        assert!(connection
            .prepare("select 1 where 0")
            .unwrap()
            .step()
            .unwrap()
            .is_none());
        let row = connection
            .prepare("select NULL")
            .unwrap()
            .step()
            .unwrap()
            .unwrap();
        assert!(matches!(row[0], Value::Null));
    }

    #[test]
    fn sqlite_typed_bindings_preserve_values_after_finalize() {
        let connection = SqliteConnection::open(":memory:").unwrap();
        let statement = connection.prepare("select ?, ?, ?, ?, ?, ?, ?").unwrap();
        statement.bind_i64(1, i64::MIN).unwrap();
        statement.bind_i64(2, i64::MAX).unwrap();
        statement.bind_f64(3, -1.25).unwrap();
        statement.bind_text(4, "a\0b").unwrap();
        statement.bind_blob(5, &[]).unwrap();
        statement.bind_blob(6, &[0, 255, 128]).unwrap();
        statement.bind_null(7).unwrap();
        assert!(statement.bind_f64(3, f64::NAN).is_err());
        let row = statement.step().unwrap().unwrap();
        assert!(statement.step().unwrap().is_none());
        statement.finalize().unwrap();
        drop(statement);
        drop(connection);
        assert!(matches!(row[0], Value::Integer(i64::MIN)));
        assert!(matches!(row[1], Value::Integer(i64::MAX)));
        assert!(matches!(row[2], Value::Real(value) if value == -1.25));
        assert!(matches!(&row[3], Value::Text(value) if value == "a\0b"));
        assert!(matches!(&row[4], Value::Blob(value) if value.is_empty()));
        assert!(matches!(&row[5], Value::Blob(value) if value == &[0, 255, 128]));
        assert!(matches!(row[6], Value::Null));
    }

    #[test]
    fn sqlite_explicit_finalize_and_drop_finalize_once() {
        use std::sync::atomic::Ordering;
        let connection = SqliteConnection::open(":memory:").unwrap();
        let statement = connection.prepare("select 1").unwrap();
        let finalized = Arc::clone(&statement.finalized);
        statement.finalize().unwrap();
        statement.finalize().unwrap();
        assert!(statement.step().is_err());
        drop(statement);
        assert_eq!(finalized.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn sqlite_serializes_step_and_changes_across_statements() {
        let connection = SqliteConnection::open(":memory:").unwrap();
        connection
            .prepare("create table t (n integer)")
            .unwrap()
            .execute()
            .unwrap();
        let threads = [1, 2].map(|count| {
            let connection = Arc::clone(&connection);
            std::thread::spawn(move || {
                for _ in 0..40 {
                    let sql = if count == 1 {
                        "insert into t values (1)"
                    } else {
                        "insert into t values (2), (3)"
                    };
                    assert_eq!(connection.prepare(sql).unwrap().execute().unwrap(), count);
                }
            })
        });
        for thread in threads {
            thread.join().unwrap();
        }
        let row = connection
            .prepare("select count(*) from t")
            .unwrap()
            .step()
            .unwrap()
            .expect("row");
        assert!(matches!(&row[0], Value::Integer(value) if *value == 120));
    }
}
