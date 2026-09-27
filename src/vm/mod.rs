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

#[cfg(test)]
mod unsigned_tests;

#[cfg(test)]
mod bitwise_tests;

#[cfg(test)]
mod conversion_tests;

#[cfg(test)]
mod generic_tests;
