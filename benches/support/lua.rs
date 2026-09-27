//! Opt-in, same-process Lua adapter; no Lua dependency for ordinary test runs.
use super::{invalid, BenchResult};
use libloading::Library;
use std::{
    cell::RefCell,
    ffi::{c_char, c_int, c_void, CStr, OsStr},
    fs,
    path::Path,
    process::Command,
    ptr::NonNull,
};

pub const SOURCE: &str = include_str!("../programs/while.lua");
pub const BRIDGE: &str = include_str!("lua_bridge.c");
type Create = unsafe extern "C" fn(*const u8, usize, *mut c_char, usize) -> *mut c_void;
type Call =
    unsafe extern "C" fn(*mut c_void, c_int, i32, i32, *mut i32, *mut c_char, usize) -> c_int;
type Destroy = unsafe extern "C" fn(*mut c_void);
type Release = unsafe extern "C" fn() -> *const c_char;

pub struct Lua {
    // A borrow excludes overlapping access to the single-threaded Lua state.
    state: RefCell<NonNull<c_void>>,
    call: Call,
    destroy: Destroy,
    pub metadata: String,
    // Drop closes the state before this library (and its Lua dependency) unloads.
    _library: Library,
}

fn pkg_config(package: &OsStr, arguments: &[&str]) -> BenchResult<String> {
    let output = Command::new("pkg-config")
        .args(arguments)
        .arg(package)
        .output()
        .map_err(|error| invalid(format!("--lua requires pkg-config: {error}")))?;
    if !output.status.success() {
        return Err(invalid(format!(
            "Lua pkg-config failed (set LUA_PKG_CONFIG to the installed package): {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn error_text(buffer: &[c_char]) -> String {
    let bytes: Vec<u8> = buffer
        .iter()
        .take_while(|&&value| value != 0)
        .map(|&value| value as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

impl Lua {
    pub fn load(directory: &Path, cc: &OsStr) -> BenchResult<Self> {
        let package = std::env::var_os("LUA_PKG_CONFIG").unwrap_or_else(|| "lua".into());
        let version = pkg_config(&package, &["--modversion"])?;
        let flags = pkg_config(&package, &["--cflags", "--libs"])?;
        // pkg-config escapes whitespace-containing paths. Do not silently split
        // those into incorrect arguments or execute a shell to interpret them.
        if flags.contains(['\\', '\'', '"']) {
            return Err(invalid("Lua pkg-config flags with shell escapes/quotes are unsupported; use a prefix without spaces"));
        }
        let bridge = directory.join("lua_bridge.c");
        let path = directory.join(if cfg!(target_os = "macos") {
            "lua_bridge.dylib"
        } else {
            "lua_bridge.so"
        });
        fs::write(&bridge, BRIDGE)?;
        let output = Command::new(cc)
            .args(["-std=c11", "-O3", "-Wall", "-Wextra", "-Werror", "-fPIC"])
            .arg(if cfg!(target_os = "macos") {
                "-dynamiclib"
            } else {
                "-shared"
            })
            .arg(&bridge)
            .args(flags.split_whitespace())
            .arg("-o")
            .arg(&path)
            .output()?;
        if !output.status.success() {
            return Err(invalid(format!(
                "Lua bridge compilation failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        // SAFETY: The library is built from the trusted bridge above against
        // installed Lua headers. It remains owned until after the state closes.
        let library = unsafe { Library::new(&path)? };
        // SAFETY: Signatures match lua_bridge.c; all symbols share its lifetime.
        let (create, call, destroy, release) = unsafe {
            (
                *library.get::<Create>(b"joky_bench_lua_create")?,
                *library.get::<Call>(b"joky_bench_lua_call")?,
                *library.get::<Destroy>(b"joky_bench_lua_destroy")?,
                *library.get::<Release>(b"joky_bench_lua_release")?,
            )
        };
        // SAFETY: The bridge returns a non-null static NUL-terminated string.
        let release = unsafe { CStr::from_ptr(release()) }.to_string_lossy();
        let mut error = [0; 512];
        // SAFETY: Buffers are valid for their supplied lengths, creation catches
        // Lua errors in C, and the returned state has a unique owner below.
        let state = unsafe {
            create(
                SOURCE.as_ptr(),
                SOURCE.len(),
                error.as_mut_ptr(),
                error.len(),
            )
        };
        let state = NonNull::new(state).ok_or_else(|| invalid(error_text(&error)))?;
        let link_info = Command::new(if cfg!(target_os = "macos") {
            "otool"
        } else {
            "ldd"
        })
        .args(if cfg!(target_os = "macos") {
            vec![OsStr::new("-L"), path.as_os_str()]
        } else {
            vec![path.as_os_str()]
        })
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).replace('\n', " | "))
        .unwrap_or_else(|| "unavailable".into());
        Ok(Self {
            state: RefCell::new(state),
            call,
            destroy,
            metadata: format!("lua_package={}\nlua_pkg_version={version}\nlua_headers={release}\nlua_integer_bits=64\nlua_pkg_flags={flags}\nlua_bridge_cc_flags=-std=c11 -O3 -Wall -Wextra -Werror -fPIC\nlua_linkage={link_info}\nlua_call_boundary=cached function, protected lua_pcall, checked integer return\nlua_arithmetic=native 64-bit with final Int32 conversion; mask32 adds per-body low-32-bit mask\n", package.to_string_lossy()),
            _library: library,
        })
    }

    pub fn call(&self, kernel: usize, n: i32, seed: i32) -> BenchResult<i32> {
        if kernel >= 6 {
            return Err(invalid("Invalid Lua benchmark kernel"));
        }
        let state = self.state.borrow_mut();
        let mut result = 0;
        let mut error = [0; 512];
        // SAFETY: The state is exclusively borrowed and its library is alive.
        // C catches Lua failures before returning; output buffers are writable.
        let status = unsafe {
            (self.call)(
                state.as_ptr(),
                kernel as c_int,
                n,
                seed,
                &mut result,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        if status != 0 {
            return Err(invalid(error_text(&error)));
        }
        Ok(result)
    }
}

impl Drop for Lua {
    fn drop(&mut self) {
        // SAFETY: This uniquely owned state is live and cannot be borrowed while
        // dropping. The library is released only after this destructor returns.
        unsafe { (self.destroy)(self.state.get_mut().as_ptr()) };
    }
}
