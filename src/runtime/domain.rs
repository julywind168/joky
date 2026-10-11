//! Compiler-owned source domain adapters and versioned module metadata.
use crate::{Error, ErrorCode, ErrorPhase, FunctionSignature, Type};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub(crate) enum Operation {
    Create,
    Handle,
    ReadOnly,
    Close,
    Request,
    Read,
    Write,
}
pub(crate) fn operation(name: &str) -> Option<Operation> {
    Some(match name {
        "__domain_create" => Operation::Create,
        "__domain_handle" => Operation::Handle,
        "__domain_readonly" => Operation::ReadOnly,
        "__domain_close" => Operation::Close,
        "__domain_request" => Operation::Request,
        "__domain_read_at" => Operation::Read,
        "__domain_write_at" => Operation::Write,
        _ => return None,
    })
}
pub(crate) fn module_type() -> Type {
    Type::Named("domain.Module".into())
}
pub(crate) fn owner_type() -> Type {
    Type::Resource("domain.StateOwner".into())
}
pub(crate) fn handle_type() -> Type {
    Type::Named("domain.Handle".into())
}
impl Operation {
    pub(crate) fn signature(self) -> FunctionSignature {
        let token = handle_type();
        let text = Type::String;
        let i = Type::Int32;
        let m = module_type();
        let owner = owner_type();
        let (params, ret) = match self {
            Self::Create => (vec![m, text, i], owner),
            Self::Handle => (vec![Type::reference(owner, false)], token),
            Self::ReadOnly => (vec![token.clone()], token),
            Self::Close => (vec![Type::reference(owner, true)], Type::Unit),
            Self::Request => (vec![m, token, text, i], Type::Int64),
            Self::Read => (vec![m, token, text], i),
            Self::Write => (vec![m, token, text, i.clone()], i),
        };
        FunctionSignature::new(params, ret)
    }
    pub(crate) fn validate(
        self,
        signature: &FunctionSignature,
        suspends: bool,
    ) -> Result<(), Error> {
        if !suspends || *signature != self.signature() {
            return Err(Error::new_with_code(
                ErrorPhase::TypeCheck,
                ErrorCode::InvalidCall,
                "Domain operation requires its exact compiler-owned signature",
            ));
        }
        Ok(())
    }
}

pub(crate) mod metadata;

#[export_name = "__joky_domain_module_placeholder"]
pub(crate) extern "C" fn module_placeholder() -> u64 {
    0
}
#[export_name = "__joky_domain_poll_v1"]
/// # Safety
/// `frame` must be a live, uniquely polled CPS frame with an attached arena and
/// slots matching `kind`'s exact compiler-owned signature. Code and borrowed
/// resources must remain live through completion or stop on this thread.
pub(crate) unsafe extern "C" fn poll_native(frame: *mut super::cps::CpsFrame, kind: u32) -> i32 {
    let op = match kind {
        0 => Operation::Create,
        1 => Operation::Handle,
        2 => Operation::ReadOnly,
        3 => Operation::Close,
        4 => Operation::Request,
        5 => Operation::Read,
        6 => Operation::Write,
        _ => {
            super::error::record_error(Error::new_with_code(
                ErrorPhase::Runtime,
                ErrorCode::AbiError,
                "Unknown domain operation",
            ));
            return super::cps::RUNTIME_ERROR;
        }
    };
    match poll(&mut *super::cps::arena(frame), &mut *frame, op) {
        Ok(Some(value)) => {
            (*frame).result = value;
            0
        }
        Ok(None) => 1,
        Err(error) => {
            super::error::record_error(error);
            super::cps::RUNTIME_ERROR
        }
    }
}
pub(crate) use crate::vm::state_domains::source::poll;
