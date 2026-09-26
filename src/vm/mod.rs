//! Public VM embedding interface.
mod engine;
pub use engine::VmEngine;

#[cfg(test)]
mod tests;
