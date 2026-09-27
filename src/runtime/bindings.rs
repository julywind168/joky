//! Native ABI entries shared by the VM, module registry, and interpreter.
use super::cps::CpsEntry;
use crate::FunctionSignature;
use std::{any::Any, rc::Rc};

#[derive(Clone)]
pub(crate) enum HostEntry {
    Direct(*const u8),
    Cps {
        entry: CpsEntry,
        frame_slots: Option<usize>,
    },
}

#[derive(Clone)]
pub(crate) struct HostBinding {
    pub signature: FunctionSignature,
    pub entry: HostEntry,
    pub owner: Option<Rc<dyn Any>>,
}
