//! Public VM embedding interface.
mod engine;
pub use engine::{VmEngine, VmMode};

#[cfg(test)]
mod tests;
