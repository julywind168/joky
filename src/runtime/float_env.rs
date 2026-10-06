//! Thread-local floating-point state at compiler, execution and host boundaries.
//!
//! Joky uses nearest-even rounding, gradual underflow and masked exceptions.
//! Save status flags as well as control bits: an invocation must not modify the
//! embedding thread's environment. Guards belong to one native stack activation,
//! never to a suspended continuation or a different thread.
use std::{marker::PhantomData, rc::Rc};

#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
compile_error!("Joky floating-point environment isolation requires aarch64 or x86_64");

#[cfg(target_arch = "aarch64")]
mod platform {
    pub(super) fn read() -> u64 {
        let control: u64;
        let status: u64;
        // These unprivileged registers contain only thread-local FP state.
        unsafe {
            std::arch::asm!("mrs {c}, fpcr", "mrs {s}, fpsr", c = out(reg) control, s = out(reg) status);
        }
        control | (status << 32)
    }
    pub(super) unsafe fn write(state: u64) {
        std::arch::asm!("msr fpcr, {c}", "msr fpsr, {s}", c = in(reg) state & 0xffff_ffff, s = in(reg) state >> 32);
    }
    pub(super) const STANDARD: u64 = 0;
}

#[cfg(target_arch = "x86_64")]
mod platform {
    // Rust and Cranelift scalar f32/f64 operations use SSE, not the x87 stack.
    pub(super) fn read() -> u64 {
        let mut state = 0u32;
        unsafe {
            std::arch::asm!("stmxcsr [{p}]", p = in(reg) &mut state);
        }
        u64::from(state)
    }
    pub(super) unsafe fn write(state: u64) {
        let state = state as u32;
        std::arch::asm!("ldmxcsr [{p}]", p = in(reg) &state);
    }
    pub(super) const STANDARD: u64 = 0x1f80;
}

pub(crate) struct Guard {
    saved: u64,
    _thread: PhantomData<Rc<()>>,
}
impl Guard {
    pub(crate) fn enter() -> Self {
        Self {
            saved: enter(),
            _thread: PhantomData,
        }
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        // Only a value read from this thread's registers can enter a guard.
        unsafe {
            restore(self.saved);
        }
    }
}

/// Private native ABI. The returned token must be restored on every exit.
#[export_name = "__joky_float_env_enter"]
pub(crate) extern "C" fn enter() -> u64 {
    let saved = platform::read();
    reset();
    saved
}

/// Reestablish Joky's environment after an arbitrary host callback returns.
#[export_name = "__joky_float_env_reset"]
pub(crate) extern "C" fn reset() {
    // The standard value contains no reserved bits or enabled traps.
    unsafe {
        platform::write(platform::STANDARD);
    }
}

/// # Safety
/// `saved` must be an unchanged token from `enter` on the same thread.
#[export_name = "__joky_float_env_restore"]
pub(crate) unsafe extern "C" fn restore(saved: u64) {
    platform::write(saved);
}

pub(crate) fn symbols() -> [(&'static str, *const u8); 3] {
    [
        ("__joky_float_env_enter", enter as *const u8),
        ("__joky_float_env_reset", reset as *const u8),
        ("__joky_float_env_restore", restore as *const u8),
    ]
}

#[cfg(test)]
pub(crate) mod testing {
    pub(crate) fn state() -> u64 {
        super::platform::read()
    }
    pub(crate) fn hostile() {
        #[cfg(target_arch = "aarch64")]
        let bits = (1 << 24) | (1 << 22) | (16 << 32); // FZ and round toward +infinity.
        #[cfg(target_arch = "x86_64")]
        let bits = 0x1f80 | (1 << 15) | (1 << 6) | (2 << 13) | (1 << 5); // FTZ, DAZ, +infinity.
        unsafe {
            super::platform::write(bits);
        }
    }
}
