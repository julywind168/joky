use crate::diagnostic::CodegenError;
use crate::sema::Type;

pub(super) fn method_key(receiver_type: Type, name: &str) -> String {
    match receiver_type {
        Type::Class(id) => format!("class_{id}_{name}"),
        Type::Struct(id) => format!("struct_{id}_{name}"),
        _ => unreachable!("method keys require a class or struct type"),
    }
}

pub(super) fn codegen_error(error: impl ToString) -> CodegenError {
    CodegenError::BackendInitialization {
        message: error.to_string(),
    }
}
