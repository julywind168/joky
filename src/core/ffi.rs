//! Synchronous C references and opaque Ptr values, separate from the legacy host ABI.
use crate::core::types::{ConstArgument, FixedArrayType, Type};
use crate::{
    parser::ast::{
        Attribute, AttributeArgument, AttributeValue, ExternFnDecl, Item, Module, RecordDef,
    },
    Error, ErrorCode, ErrorPhase, FunctionSignature, Span, TypeRegistry,
};
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
            .any(|ty| !matches!(ty, Type::Reference(_)) || ty.contains_fixed_array())
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
    // Resolve shallow record identities too: array fields must not reach a host
    // that can retain inputs across Pending.
    for ty in &signature.params {
        let target =
            super::types::reference_target(&ty.reference_type().unwrap().target, nominals, types)?;
        if target.contains_fixed_array() {
            return Err(error(
                span,
                "Borrowing host v1 does not accept fixed arrays or records containing them",
            ));
        }
    }
    // Remaining layouts and permissions match synchronous C references.
    validate_signature(signature, false, nominals, types, span)
}

pub(crate) fn error(span: Span, message: impl Into<String>) -> Error {
    Error::new_with_code(ErrorPhase::Abi, ErrorCode::AbiError, message).with_span(span)
}

pub(crate) fn validate_attributes(record: &RecordDef) -> Result<(), Error> {
    let mut seen = std::collections::HashSet::new();
    for attribute in &record.attributes {
        if !seen.insert(&attribute.path) {
            return Err(error(
                attribute.span,
                format!("Duplicate #{} attribute", attribute.path),
            ));
        }
        if attribute.path == "state" {
            if attribute.arguments.is_some() || !record.type_params.is_empty() {
                return Err(error(
                    attribute.span,
                    "#state requires a nongeneric struct and accepts no arguments",
                ));
            }
            continue;
        }
        if attribute.path != "repr" {
            return Err(Error::new_with_code(
                ErrorPhase::TypeCheck,
                ErrorCode::UnknownName,
                format!("Unknown attribute #{}", attribute.path),
            )
            .with_span(attribute.span));
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

/// Inline C members are fixed-width numerics, validated C records or nonempty
/// arrays of those types. Floats match the platform's IEEE binary32/binary64 C layout and
/// narrow integers match the C char/short lanes; `target_c_layout` tests
/// assert size, alignment and offsets against a real C compiler.
pub(crate) fn is_c_value(ty: &Type) -> bool {
    c_target(ty, false, 0)
}

// Registration can name source records inside arrays; loading resolves their
// identity before checking the complete layout and exact source signature.
fn c_target(ty: &Type, names: bool, depth: usize) -> bool {
    if depth >= 256 {
        return false;
    }
    match ty {
        Type::FixedArray(a) => {
            matches!(a.length, ConstArgument::Value(1..=65_536))
                && c_target(&a.element, names, depth + 1)
        }
        Type::Record(r) => r.repr_c && r.parameters.is_empty(),
        Type::Named(name) if names => !name.is_empty() && !name.contains('\0'),
        _ => ty.integer().is_some() && *ty != Type::Rune || ty.is_float(),
    }
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
                "C references require fixed-width numerics, #repr(C) records, or nonempty arrays of C-compatible elements",
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
        || signature.params.iter().any(|ty| {
            *ty != Type::Ptr
                && !matches!(ty, Type::Reference(r)
            if c_target(&r.target, true, 0))
        })
    {
        return Err(error(Span::empty(0), "C registration requires at most four Ptr or references to C-compatible numerics, records or nonempty arrays and an integer, Ptr or Unit result"));
    }
    Ok(())
}

/// Resolve registration names in this module, keeping nominal identity checks.
pub(crate) fn resolve_registration(
    signature: &FunctionSignature,
    nominals: &HashMap<String, Type>,
) -> Result<FunctionSignature, Error> {
    fn resolve(ty: &Type, nominals: &HashMap<String, Type>) -> Result<Type, Error> {
        match ty {
            Type::Named(name) => nominals.get(name).cloned().ok_or_else(|| {
                error(
                    Span::empty(0),
                    format!("Unknown C registration record {name}"),
                )
            }),
            Type::FixedArray(a) => Ok(Type::FixedArray(std::sync::Arc::new(FixedArrayType {
                element: resolve(&a.element, nominals)?,
                length: a.length.clone(),
            }))),
            _ => Ok(ty.clone()),
        }
    }
    let mut signature = signature.clone();
    for ty in &mut signature.params {
        if let Type::Reference(reference) = ty {
            let target = resolve(&reference.target, nominals)?;
            if !is_c_value(&target) {
                return Err(error(
                    Span::empty(0),
                    "C registration target is not C-compatible",
                ));
            }
            target
                .memory_layout()
                .map_err(|e| error(Span::empty(0), e.to_string()))?;
            *ty = Type::reference(target, reference.mutable);
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
            Type::Int8 => "int8_t".into(),
            Type::Int16 => "int16_t".into(),
            Type::Int32 => "int32_t".into(),
            Type::Int64 => "int64_t".into(),
            Type::UInt8 => "uint8_t".into(),
            Type::UInt16 => "uint16_t".into(),
            Type::UInt32 => "uint32_t".into(),
            Type::UInt64 => "uint64_t".into(),
            Type::Float32 => "float".into(),
            Type::Float64 => "double".into(),
            Type::Unit => "void".into(),
            Type::Ptr => "void *".into(),
            Type::Record(r) if r.repr_c => ident(&r.name, span)?,
            _ => return Err(error(span, "Unsupported C header type")),
        })
    }
    // Build the declarator from the outside in: pointer-to-array parentheses
    // belong around the name, while dimensions retain source order.
    fn declaration(ty: &Type, name: &str, span: Span) -> Result<String, Error> {
        match ty {
            Type::FixedArray(a) => declaration(&a.element, &format!("{name}[{}]", a.length), span),
            Type::Reference(r) => {
                let name = if matches!(r.target, Type::FixedArray(_)) {
                    format!("(*{name})")
                } else {
                    format!("*{name}")
                };
                Ok(format!(
                    "{}{}",
                    if r.mutable { "" } else { "const " },
                    declaration(&r.target, &name, span)?
                ))
            }
            _ => Ok(format!("{} {name}", type_name(ty, span)?)),
        }
    }
    fn array_assertions(
        ty: &Type,
        expression: &str,
        span: Span,
        output: &mut String,
    ) -> Result<(), Error> {
        let Type::FixedArray(a) = ty else {
            return Ok(());
        };
        let layout = ty.memory_layout().map_err(|e| error(span, e.to_string()))?;
        let MemoryLayoutShape::Array { stride, length, .. } = layout.shape else {
            unreachable!()
        };
        let element = format!("({expression})[0]");
        writeln!(
            output,
            "_Static_assert(sizeof({expression}) == {}, \"array size mismatch\");",
            layout.size
        )
        .unwrap();
        writeln!(
            output,
            "_Static_assert(_Alignof({}) == {}, \"array alignment mismatch\");",
            declaration(ty, "", span)?,
            layout.align
        )
        .unwrap();
        writeln!(
            output,
            "_Static_assert(sizeof({element}) == {stride}, \"array stride mismatch\");"
        )
        .unwrap();
        writeln!(output, "_Static_assert(sizeof({expression}) / sizeof({element}) == {length}, \"array dimension mismatch\");").unwrap();
        array_assertions(&a.element, &element, span, output)
    }
    fn record(
        ty: &Type,
        span: Span,
        output: &mut String,
        done: &mut HashSet<u64>,
        names: &mut HashSet<String>,
    ) -> Result<(), Error> {
        if let Type::FixedArray(a) = ty {
            return record(&a.element, span, output, done, names);
        }
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
                "    {};",
                declaration(&field.ty, &ident(&field.name, span)?, span)?
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
            array_assertions(&field.ty, &format!("(({name} *)0)->{member}"), span, output)?;
        }
        output.push('\n');
        Ok(())
    }
    let mut output = String::from("/* Generated by Joky for the native target; compile as C11. */\n#pragma once\n#include <stdint.h>\n#include <stddef.h>\n\n");
    let mut done = HashSet::new();
    let mut names = HashSet::from([
        "int8_t".into(),
        "uint8_t".into(),
        "int16_t".into(),
        "uint16_t".into(),
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
        if super::builtins::ExternAbi::classify(
            f,
            &types,
            super::builtins::Builtin::lookup(&f.name).as_ref(),
        ) == super::builtins::ExternAbi::Borrowing
        {
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
        for ty in &signature.params {
            if let Type::Reference(r) = ty {
                let target = super::types::reference_target(&r.target, &nominals, &types)?;
                record(&target, f.span, &mut output, &mut done, &mut names)?;
                if matches!(target, Type::FixedArray(_)) {
                    let pointer = declaration(&Type::reference(target.clone(), false), "", f.span)?;
                    array_assertions(&target, &format!("*(({pointer})0)"), f.span, &mut output)?;
                }
            }
        }
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
            let target = super::types::reference_target(&r.target, &nominals, &types)?;
            write!(
                output,
                "{}",
                declaration(
                    &Type::reference(target, r.mutable),
                    &format!("arg{index}"),
                    f.span
                )?
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
