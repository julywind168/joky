//! Validation of compile-time payloads for resumable handlers.

use super::*;

pub(super) fn verify_constant_statement(
    function: &MirFunction,
    destination: MirValueId,
    value: &MirConstant,
) -> Result<(), Diagnostic> {
    let destination_type = check_value_exists(function, destination)?;
    let expected = match value {
        MirConstant::EffectOperation(_) => Type::U64,
        MirConstant::Integer(_) => {
            if !destination_type.is_integer() && destination_type != Type::Duration {
                return Err(Diagnostic::codegen(format!(
                    "MIR function '{}' gives an integer constant a non-integer type",
                    function.name
                )));
            }
            destination_type
        }
        MirConstant::Float(_) => {
            if !destination_type.is_float() {
                return Err(Diagnostic::codegen(format!(
                    "MIR function '{}' gives a float constant a non-float type",
                    function.name
                )));
            }
            destination_type
        }
        MirConstant::String(_) => Type::String,
        MirConstant::Boolean(_) => Type::Bool,
        MirConstant::Option(_) => {
            if !matches!(destination_type, Type::Option(_)) {
                return Err(Diagnostic::codegen(format!(
                    "MIR function '{}' gives an Option constant a non-Option type",
                    function.name
                )));
            }
            destination_type
        }
        MirConstant::Result { .. } => {
            if !matches!(destination_type, Type::Result(_)) {
                return Err(Diagnostic::codegen(format!(
                    "MIR function '{}' gives a Result constant a non-Result type",
                    function.name
                )));
            }
            destination_type
        }
        MirConstant::Tuple(_) => {
            if !matches!(destination_type, Type::Tuple(_)) {
                return Err(Diagnostic::codegen(format!(
                    "MIR function '{}' gives a Tuple constant a non-Tuple type",
                    function.name
                )));
            }
            destination_type
        }
        MirConstant::Struct(_) => {
            if !matches!(destination_type, Type::Struct(_)) {
                return Err(Diagnostic::codegen(format!(
                    "MIR function '{}' gives a Struct constant a non-Struct type",
                    function.name
                )));
            }
            destination_type
        }
        MirConstant::Class(_) => {
            if !matches!(destination_type, Type::Class(_)) {
                return Err(Diagnostic::codegen(format!(
                    "MIR function '{}' gives a Class constant a non-Class type",
                    function.name
                )));
            }
            destination_type
        }
        MirConstant::List(_) => {
            if !matches!(destination_type, Type::List(_)) {
                return Err(Diagnostic::codegen(format!(
                    "MIR function '{}' gives a List constant a non-List type",
                    function.name
                )));
            }
            destination_type
        }
        MirConstant::Map(_) => {
            if !matches!(destination_type, Type::Map(_)) {
                return Err(Diagnostic::codegen(format!(
                    "MIR function '{}' gives a Map constant a non-Map type",
                    function.name
                )));
            }
            destination_type
        }
        MirConstant::MutList(_) => {
            if !matches!(destination_type, Type::MutList(_)) {
                return Err(Diagnostic::codegen(format!(
                    "MIR function '{}' gives a MutList constant a non-MutList type",
                    function.name
                )));
            }
            destination_type
        }
        MirConstant::MutMap(_) => {
            if !matches!(destination_type, Type::MutMap(_)) {
                return Err(Diagnostic::codegen(format!(
                    "MIR function '{}' gives a MutMap constant a non-MutMap type",
                    function.name
                )));
            }
            destination_type
        }
        MirConstant::MutSet(_) => {
            if !matches!(destination_type, Type::MutSet(_)) {
                return Err(Diagnostic::codegen(format!(
                    "MIR function '{}' gives a MutSet constant a non-MutSet type",
                    function.name
                )));
            }
            destination_type
        }
        MirConstant::Enum { .. } => {
            if !matches!(destination_type, Type::Enum(_)) {
                return Err(Diagnostic::codegen(format!(
                    "MIR function '{}' gives an Enum constant a non-Enum type",
                    function.name
                )));
            }
            destination_type
        }
    };
    check_destination_type(function, destination, expected)
}

pub(super) fn verify_resumable_constant(
    constant: &MirConstant,
    expected: Type,
    types: &TypeTable,
) -> Result<(), Diagnostic> {
    match constant {
        MirConstant::Integer(_) if expected.is_integer() || expected == Type::Duration => Ok(()),
        MirConstant::Float(_) if expected.is_float() => Ok(()),
        MirConstant::String(_) if expected == Type::String => Ok(()),
        MirConstant::Boolean(_) if expected == Type::Bool => Ok(()),
        MirConstant::Option(value) => {
            let Type::Option(id) = expected else {
                return Err(Diagnostic::codegen(
                    "MIR resumable Option payload has a non-Option result type",
                ));
            };
            if let Some(value) = value {
                verify_resumable_constant(value, types.option_type(id), types)?;
            }
            Ok(())
        }
        MirConstant::Result { is_ok, value } => {
            let Type::Result(id) = expected else {
                return Err(Diagnostic::codegen(
                    "MIR resumable Result payload has a non-Result result type",
                ));
            };
            let (ok, err) = types.result_types(id);
            verify_resumable_constant(value, if *is_ok { ok } else { err }, types)
        }
        MirConstant::Tuple(values) => {
            let Type::Tuple(id) = expected else {
                return Err(Diagnostic::codegen(
                    "MIR resumable Tuple payload has a non-Tuple result type",
                ));
            };
            let elements = types.tuple_elements(id);
            if values.len() != elements.len() {
                return Err(Diagnostic::codegen(
                    "MIR resumable Tuple payload element count mismatch",
                ));
            }
            for (value, ty) in values.iter().zip(elements.iter().copied()) {
                verify_resumable_constant(value, ty, types)?;
            }
            Ok(())
        }
        MirConstant::Struct(values) => {
            let Type::Struct(id) = expected else {
                return Err(Diagnostic::codegen(
                    "MIR resumable Struct payload has a non-Struct result type",
                ));
            };
            let fields = types.struct_fields(id);
            if values.len() != fields.len()
                || fields
                    .iter()
                    .any(|(name, _)| !values.iter().any(|(value_name, _)| value_name == name))
            {
                return Err(Diagnostic::codegen(
                    "MIR resumable Struct payload fields do not match its type",
                ));
            }
            for (name, ty) in fields {
                let value = values
                    .iter()
                    .find(|(value_name, _)| value_name == name)
                    .expect("struct field checked above")
                    .1
                    .clone();
                verify_resumable_constant(&value, *ty, types)?;
            }
            Ok(())
        }
        MirConstant::Class(values) => {
            let Type::Class(id) = expected else {
                return Err(Diagnostic::codegen(
                    "MIR resumable Class payload has a non-Class result type",
                ));
            };
            let fields = types.class_fields(id);
            if values.len() != fields.len()
                || fields
                    .iter()
                    .any(|(name, _)| !values.iter().any(|(value_name, _)| value_name == name))
            {
                return Err(Diagnostic::codegen(
                    "MIR resumable Class payload fields do not match its type",
                ));
            }
            for (name, ty) in fields {
                let value = values
                    .iter()
                    .find(|(value_name, _)| value_name == name)
                    .expect("class field checked above")
                    .1
                    .clone();
                verify_resumable_constant(&value, *ty, types)?;
            }
            Ok(())
        }
        MirConstant::List(values) => {
            let Type::List(id) = expected else {
                return Err(Diagnostic::codegen(
                    "MIR resumable List payload has a non-List result type",
                ));
            };
            let element = types.list_type(id);
            for value in values {
                verify_resumable_constant(value, element, types)?;
            }
            Ok(())
        }
        MirConstant::Map(entries) => {
            let Type::Map(id) = expected else {
                return Err(Diagnostic::codegen(
                    "MIR resumable Map payload has a non-Map result type",
                ));
            };
            let info = types.map_info(id);
            for (key, value) in entries {
                verify_resumable_constant(key, info.key, types)?;
                verify_resumable_constant(value, info.value, types)?;
            }
            Ok(())
        }
        MirConstant::MutList(values) => {
            let Type::MutList(id) = expected else {
                return Err(Diagnostic::codegen(
                    "MIR resumable MutList payload has a non-MutList result type",
                ));
            };
            let element = types.list_type(id);
            for value in values {
                verify_resumable_constant(value, element, types)?;
            }
            Ok(())
        }
        MirConstant::MutMap(entries) => {
            let Type::MutMap(id) = expected else {
                return Err(Diagnostic::codegen(
                    "MIR resumable MutMap payload has a non-MutMap result type",
                ));
            };
            let info = types.map_info(id);
            for (key, value) in entries {
                verify_resumable_constant(key, info.key, types)?;
                verify_resumable_constant(value, info.value, types)?;
            }
            Ok(())
        }
        MirConstant::MutSet(values) => {
            let Type::MutSet(id) = expected else {
                return Err(Diagnostic::codegen(
                    "MIR resumable MutSet payload has a non-MutSet result type",
                ));
            };
            let info = types.map_info(id);
            for value in values {
                verify_resumable_constant(value, info.key, types)?;
            }
            Ok(())
        }
        MirConstant::Enum { variant, fields } => {
            let Type::Enum(id) = expected else {
                return Err(Diagnostic::codegen(
                    "MIR resumable Enum payload has a non-Enum result type",
                ));
            };
            let variants = types.enum_variants(id);
            let layout = variants.get(*variant).ok_or_else(|| {
                Diagnostic::codegen("MIR resumable Enum payload variant is out of bounds")
            })?;
            if fields.len() != layout.fields.len() {
                return Err(Diagnostic::codegen(
                    "MIR resumable Enum payload field count mismatch",
                ));
            }
            for (value, (_, ty)) in fields.iter().zip(&layout.fields) {
                verify_resumable_constant(value, *ty, types)?;
            }
            Ok(())
        }
        _ => Err(Diagnostic::codegen(
            "MIR resumable handler payload does not match its result type",
        )),
    }
}
