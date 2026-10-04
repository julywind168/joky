//! Synchronous C references and opaque Ptr values, separate from the legacy host ABI.
use crate::{parser::ast::*, Error, ErrorCode, ErrorPhase, FunctionSignature, Span, TypeRegistry};
use std::collections::HashMap;

/// The initial portable adapter supports zero through four pointer inputs.
pub const MAX_REFERENCE_PARAMS: usize = 4;

/// A borrowing declaration selects a distinct, versioned descriptor ABI.
pub(crate) fn is_borrowing_import(f: &ExternFnDecl) -> bool {
    !f.c_abi && f.suspends && f.params.iter().any(|p| p.ty.reference_type().is_some())
}

pub(crate) fn borrowing_symbol(symbol: &str) -> String {
    format!("__joky_cps_borrow_v1_{symbol}")
}

pub(crate) fn validate_borrowing_registration(signature: &FunctionSignature) -> Result<(), Error> {
    if signature.params.is_empty()
        || signature
            .params
            .iter()
            .any(|ty| !matches!(ty, Type::Reference(_)))
        || !(signature.ret.integer().is_some() || signature.ret == Type::Unit)
    {
        return Err(error(Span::empty(0), "Borrowing host v1 requires one to four reference parameters and an integer or Unit result"));
    }
    validate_registration(signature)
}

pub(crate) fn validate_borrowing_signature(
    signature: &FunctionSignature,
    nominals: &HashMap<String, Type>,
    types: &TypeRegistry,
    span: Span,
) -> Result<(), Error> {
    validate_borrowing_registration(signature).map_err(|e| e.with_span(span))?;
    // Layout and permissions are the same as synchronous C reference arguments.
    validate_signature(signature, false, nominals, types, span)
}

pub(crate) fn error(span: Span, message: impl Into<String>) -> Error {
    Error::new_with_code(ErrorPhase::Abi, ErrorCode::AbiError, message).with_span(span)
}

pub(crate) fn validate_attributes(record: &RecordDef) -> Result<(), Error> {
    for (index, attribute) in record.attributes.iter().enumerate() {
        if attribute.path != "repr" {
            return Err(error(
                attribute.span,
                format!("Unknown declaration attribute #{}", attribute.path),
            ));
        }
        if index != 0 {
            return Err(error(attribute.span, "Duplicate #repr attribute"));
        }
        let valid = attribute.arguments.as_deref().is_some_and(|args| matches!(args,
            [AttributeArgument { name: None, value: AttributeValue::Name(Attribute { path, arguments: None, .. }), .. }] if path == "C"));
        if !valid {
            return Err(error(attribute.span, "Only #repr(C) is supported"));
        }
        if !record.type_params.is_empty() || record.fields.is_empty() {
            return Err(error(
                record.span,
                "#repr(C) requires a nonempty, nongeneric struct",
            ));
        }
    }
    Ok(())
}

/// Inline C record members are fixed-width numerics or already validated C
/// records. Floats match the platform's IEEE binary32/binary64 C layout and
/// narrow integers match the C char/short lanes; `target_c_layout` tests
/// assert size, alignment and offsets against a real C compiler.
pub(crate) fn is_c_value(ty: &Type) -> bool {
    ty.integer().is_some() && *ty != Type::Rune
        || ty.is_float()
        || matches!(ty, Type::Record(r) if r.repr_c && r.parameters.is_empty())
}

pub(crate) fn validate_signature(
    signature: &FunctionSignature,
    suspends: bool,
    nominals: &HashMap<String, Type>,
    types: &TypeRegistry,
    span: Span,
) -> Result<(), Error> {
    if suspends
        || signature.params.len() > MAX_REFERENCE_PARAMS
        || !(signature.ret.is_numeric() || matches!(signature.ret, Type::Unit | Type::Ptr))
    {
        return Err(error(span, "C imports require at most four reference or Ptr inputs, a numeric, Ptr or Unit result, and cannot suspend"));
    }
    for ty in &signature.params {
        if *ty == Type::Ptr {
            continue;
        }
        let Type::Reference(reference) = ty else {
            return Err(error(
                span,
                "C imports accept only reference or Ptr parameters",
            ));
        };
        let target = super::types::reference_target(&reference.target, nominals, types)?;
        if !is_c_value(&target) {
            return Err(error(
                span,
                "C references require fixed-width integers or #repr(C) records",
            ));
        }
        target
            .memory_layout()
            .map_err(|e| error(span, e.to_string()))?;
    }
    Ok(())
}

pub(crate) fn validate_registration(signature: &FunctionSignature) -> Result<(), Error> {
    if signature.params.len() > MAX_REFERENCE_PARAMS
        || !(signature.ret.integer().is_some() || matches!(signature.ret, Type::Unit | Type::Ptr))
        || signature.params.iter().any(|ty| *ty != Type::Ptr && !matches!(ty, Type::Reference(r)
            if is_c_value(&r.target) || matches!(&r.target, Type::Named(name) if !name.is_empty() && !name.contains('\0'))))
    {
        return Err(error(Span::empty(0), "C registration requires at most four Ptr or references to integers or named C records and an integer, Ptr or Unit result"));
    }
    Ok(())
}

/// Resolve registration names in this module, keeping nominal identity checks.
pub(crate) fn resolve_registration(
    signature: &FunctionSignature,
    nominals: &HashMap<String, Type>,
) -> Result<FunctionSignature, Error> {
    let mut signature = signature.clone();
    for ty in &mut signature.params {
        if let Type::Reference(reference) = ty {
            if let Type::Named(name) = &reference.target {
                let target = nominals.get(name).ok_or_else(|| {
                    error(
                        Span::empty(0),
                        format!("Unknown C registration record {name}"),
                    )
                })?;
                *ty = Type::reference(target.clone(), reference.mutable);
            }
        }
    }
    Ok(signature)
}

/// Generate C11 declarations and target layout assertions for the C records and
/// imports in a source module. Uses the native target, like the JIT and AOT
/// compiler. Resolve external source modules before calling this function.
///
/// C records use their source name (qualified names replace dots/slashes with
/// underscores). Names must be portable C identifiers without collisions or
/// reserved keywords. Parameter names are generated; field names are preserved.
/// This checks layout, not pointer retention, aliasing, or the foreign body's
/// behavior: callers must still uphold the unsafe registration contract.
pub fn c_header(module: &Module) -> Result<String, Error> {
    use crate::memory_layout::MemoryLayoutShape;
    use std::{collections::HashSet, fmt::Write};
    let types = TypeRegistry::default();
    let module = types.resolve_aliases(module)?;
    let nominals = super::types::nominal_types(&types, &module)?;
    fn ident(name: &str, span: Span) -> Result<String, Error> {
        let name = name.replace(['.', '/'], "_");
        let keywords = [
            "auto",
            "break",
            "case",
            "char",
            "const",
            "continue",
            "default",
            "do",
            "double",
            "else",
            "enum",
            "extern",
            "float",
            "for",
            "goto",
            "if",
            "inline",
            "int",
            "long",
            "register",
            "restrict",
            "return",
            "short",
            "signed",
            "sizeof",
            "static",
            "struct",
            "switch",
            "typedef",
            "union",
            "unsigned",
            "void",
            "volatile",
            "while",
            "_Alignas",
            "_Alignof",
            "_Atomic",
            "_Bool",
            "_Complex",
            "_Generic",
            "_Imaginary",
            "_Noreturn",
            "_Static_assert",
            "_Thread_local",
        ];
        if name.is_empty()
            || !name.as_bytes()[0].is_ascii_alphabetic()
            || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            || keywords.contains(&name.as_str())
        {
            return Err(error(
                span,
                format!("C header requires a portable, nonreserved identifier: {name}"),
            ));
        }
        Ok(name)
    }
    fn type_name(ty: &Type, span: Span) -> Result<String, Error> {
        Ok(match ty {
            Type::Int32 => "int32_t".into(),
            Type::Int64 => "int64_t".into(),
            Type::UInt8 => "uint8_t".into(),
            Type::UInt32 => "uint32_t".into(),
            Type::UInt64 => "uint64_t".into(),
            Type::Unit => "void".into(),
            Type::Ptr => "void *".into(),
            Type::Record(r) if r.repr_c => ident(&r.name, span)?,
            _ => return Err(error(span, "Unsupported C header type")),
        })
    }
    fn record(
        ty: &Type,
        span: Span,
        output: &mut String,
        done: &mut HashSet<u64>,
        names: &mut HashSet<String>,
    ) -> Result<(), Error> {
        let Type::Record(r) = ty else { return Ok(()) };
        if !r.repr_c || !done.insert(r.id) {
            return Ok(());
        }
        for field in &r.fields {
            record(&field.ty, span, output, done, names)?;
        }
        let name = type_name(ty, span)?;
        if !names.insert(name.clone()) {
            return Err(error(span, format!("C header name collision: {name}")));
        }
        writeln!(output, "typedef struct {name} {{").unwrap();
        for field in &r.fields {
            writeln!(
                output,
                "    {} {};",
                type_name(&field.ty, span)?,
                ident(&field.name, span)?
            )
            .unwrap();
        }
        writeln!(output, "}} {name};").unwrap();
        let layout = ty.memory_layout().map_err(|e| error(span, e.to_string()))?;
        writeln!(
            output,
            "_Static_assert(sizeof({name}) == {}, \"{name} size mismatch\");",
            layout.size
        )
        .unwrap();
        writeln!(
            output,
            "_Static_assert(_Alignof({name}) == {}, \"{name} alignment mismatch\");",
            layout.align
        )
        .unwrap();
        let MemoryLayoutShape::Record(fields) = layout.shape else {
            unreachable!()
        };
        for (field, layout) in r.fields.iter().zip(fields) {
            let member = ident(&field.name, span)?;
            writeln!(output, "_Static_assert(offsetof({name}, {member}) == {}, \"{name}.{member} offset mismatch\");", layout.offset).unwrap();
        }
        output.push('\n');
        Ok(())
    }
    let mut output = String::from("/* Generated by Joky for the native target; compile as C11. */\n#pragma once\n#include <stdint.h>\n#include <stddef.h>\n\n");
    let mut done = HashSet::new();
    let mut names = HashSet::from([
        "int32_t".into(),
        "uint32_t".into(),
        "int64_t".into(),
        "uint64_t".into(),
        "size_t".into(),
        "ptrdiff_t".into(),
        "offsetof".into(),
    ]);
    for declaration in &module.records {
        if let Some(ty) = nominals.get(&declaration.name) {
            record(ty, declaration.span, &mut output, &mut done, &mut names)?;
        }
    }
    for item in &module.items {
        let Item::ExternFn(f) = item else { continue };
        if is_borrowing_import(f) {
            let signature = FunctionSignature::new(
                f.params.iter().map(|p| p.ty.clone()).collect(),
                f.ret_ty.clone(),
            );
            validate_borrowing_signature(&signature, &nominals, &types, f.span)?;
            let name = ident(&f.name, f.span)?;
            if name != f.name || !names.insert(name.clone()) {
                return Err(error(
                    f.span,
                    format!("Invalid or conflicting borrowing symbol {name}"),
                ));
            }
            writeln!(
                output,
                "#include \"joky_cps.h\"\nextern const JokyBorrowingHostV1 {};",
                borrowing_symbol(&name)
            )
            .unwrap();
            continue;
        }
        if !f.c_abi {
            continue;
        }
        let signature = FunctionSignature::new(
            f.params.iter().map(|p| p.ty.clone()).collect(),
            f.ret_ty.clone(),
        );
        validate_signature(&signature, f.suspends, &nominals, &types, f.span)?;
        let name = ident(&f.name, f.span)?;
        if name != f.name || !names.insert(name.clone()) {
            return Err(error(
                f.span,
                format!("Invalid or conflicting C symbol {name}"),
            ));
        }
        write!(output, "{} {name}(", type_name(&f.ret_ty, f.span)?).unwrap();
        for (index, ty) in signature.params.iter().enumerate() {
            if index != 0 {
                output.push_str(", ");
            }
            if *ty == Type::Ptr {
                write!(output, "void *arg{index}").unwrap();
                continue;
            }
            let Type::Reference(r) = ty else {
                unreachable!()
            };
            write!(
                output,
                "{}{} *arg{index}",
                if r.mutable { "" } else { "const " },
                type_name(&r.target, f.span)?
            )
            .unwrap();
        }
        if signature.params.is_empty() {
            output.push_str("void");
        }
        output.push_str(");\n");
    }
    Ok(output)
}

pub(crate) fn validate_text_signature(signature: &FunctionSignature) -> Result<(), Error> {
    if signature.params.len() > 4
        || signature.params.iter().any(|ty| {
            *ty != Type::String
                && !ty
                    .reference_type()
                    .is_some_and(|r| !r.mutable && r.target == Type::String)
        })
        || !(signature.ret == Type::String
            || signature.ret == Type::Unit
            || signature.ret.integer().is_some())
    {
        return Err(Error::new_with_code(ErrorPhase::Abi, ErrorCode::AbiError, "Text FFI v1 requires at most four String or &String inputs and a String, integer or Unit result"));
    }
    Ok(())
}
