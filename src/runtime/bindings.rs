//! Native ABI entries shared by the VM, module registry, and interpreter.
use super::borrowed_host::BorrowingHostV1;
use super::cps::CpsEntry;
use crate::FunctionSignature;
use std::{any::Any, rc::Rc};

#[derive(Clone)]
pub(crate) enum HostEntry {
    Direct(*const u8),
    /// Synchronous C references and opaque pointers, under a separate contract.
    C(*const u8),
    BorrowingCps(Rc<BorrowingHostV1>),
    Cps {
        entry: CpsEntry,
        frame_slots: Option<usize>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HostAbiKind {
    Scalar,
    LegacyStringV1,
}

#[derive(Clone)]
pub(crate) struct HostBinding {
    pub abi: HostAbiKind,
    pub signature: FunctionSignature,
    pub entry: HostEntry,
    pub owner: Option<Rc<dyn Any>>,
}
