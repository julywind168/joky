//! Synchronous user destructors must finish even when their owner is cancelled.
use std::cell::Cell;

pub const ENTER_SYMBOL: &str = "jk_user_drop_enter";
pub const EXIT_SYMBOL: &str = "jk_user_drop_exit";

thread_local! {
    static DEPTH: Cell<usize> = const { Cell::new(0) };
}

pub fn active() -> bool {
    DEPTH.with(|depth| depth.get() != 0)
}

// The compiler verifies that this region cannot suspend, dispatch handlers,
// panic, or unwind. Nested field/temporary destruction nests the region.
pub extern "C" fn jk_user_drop_enter() {
    DEPTH.with(|depth| depth.set(depth.get().checked_add(1).expect("Drop nesting overflow")));
}

pub extern "C" fn jk_user_drop_exit() {
    DEPTH.with(|depth| depth.set(depth.get().checked_sub(1).expect("unbalanced Drop region")));
}
