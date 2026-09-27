//! Public VM embedding interface.
mod engine;
mod limits;
pub use engine::{VmEngine, VmMode};
pub use limits::InterpreterLimits;

#[cfg(test)]
mod interpreter_tests;
#[cfg(test)]
mod tests;

#[cfg(test)]
mod int64_tests;
