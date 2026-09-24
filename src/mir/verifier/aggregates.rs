use super::*;

pub(super) fn verify_tuple_statement(
    function: &MirFunction,
    types: &TypeTable,
    available: &IndexSet<MirValueId>,
    destination: MirValueId,
    elements: &[MirValueId],
) -> Result<(), Diagnostic> {
    let destination_type = check_value_exists(function, destination)?;
    let expected: Vec<Type> = match destination_type {
        Type::Tuple(id) => types.tuple_elements(id).to_vec(),
        _ => {
            return Err(Diagnostic::codegen(format!(
                "MIR function '{}' creates an aggregate with an invalid type",
                function.name
            )))
        }
    };
    if elements.len() != expected.len() {
        return Err(Diagnostic::codegen(format!(
            "MIR function '{}' creates a tuple with {} elements, expected {}",
            function.name,
            elements.len(),
            expected.len()
        )));
    }
    for (element, expected_type) in elements.iter().zip(expected) {
        check_definition(available, *element, function)?;
        check_value_type(function, *element, expected_type)?;
        check_owned_argument(function, *element, expected_type, types, "aggregate")?;
    }
    Ok(())
}

pub(super) fn verify_enum_construct_statement(
    function: &MirFunction,
    types: &TypeTable,
    available: &IndexSet<MirValueId>,
    destination: MirValueId,
    variant: usize,
    enum_id: MirTypeId,
    arguments: &[MirCallArgument],
) -> Result<(), Diagnostic> {
    let destination_type = check_value_exists(function, destination)?;
    let fields = match enum_id {
        MirTypeId::Enum(enum_id) if destination_type == Type::Enum(enum_id) => types
            .enum_variants(enum_id)
            .get(variant)
            .map(|variant| variant.fields.as_slice()),
        MirTypeId::Option(option_id) if destination_type == Type::Option(option_id) => {
            match variant {
                0 => Some(&[("value".to_owned(), types.option_type(option_id))][..]),
                1 => Some(&[] as &[(String, Type)]),
                _ => None,
            }
        }
        MirTypeId::Result(result_id) if destination_type == Type::Result(result_id) => {
            let (ok, err) = types.result_types(result_id);
            match variant {
                0 => Some(&[("value".to_owned(), ok)][..]),
                1 => Some(&[("value".to_owned(), err)][..]),
                _ => None,
            }
        }
        _ => None,
    };
    let Some(fields) = fields else {
        return Err(Diagnostic::codegen(format!(
            "MIR function '{}' constructs a non-enum type",
            function.name
        )));
    };
    let mut ordered: Vec<Option<&MirCallArgument>> = vec![None; fields.len()];
    for argument in arguments {
        let index = argument.parameter;
        if index >= ordered.len() {
            return Err(Diagnostic::codegen("MIR enum argument index out of bounds"));
        }
        if ordered[index].is_some() {
            return Err(Diagnostic::codegen(format!(
                "MIR function '{}' passes a duplicate enum constructor argument",
                function.name
            )));
        }
        ordered[index] = Some(argument);
    }
    if ordered.iter().any(Option::is_none) {
        return Err(Diagnostic::codegen(format!(
            "MIR function '{}' passes too few enum constructor arguments",
            function.name
        )));
    }
    for ((_, field_type), argument) in fields.iter().zip(ordered.into_iter().flatten()) {
        check_definition(available, argument.value, function)?;
        check_value_type(function, argument.value, *field_type)?;
        check_owned_argument(
            function,
            argument.value,
            *field_type,
            types,
            "enum constructor",
        )?;
    }
    Ok(())
}

pub(super) fn verify_construct_statement(
    function: &MirFunction,
    types: &TypeTable,
    available: &IndexSet<MirValueId>,
    destination: MirValueId,
    fields: &[MirValueId],
    type_id: MirTypeId,
) -> Result<(), Diagnostic> {
    let destination_type = check_value_exists(function, destination)?;
    let declared = match (type_id, destination_type) {
        (MirTypeId::Struct(id), Type::Struct(destination_id)) if id == destination_id => {
            types.struct_fields(id)
        }
        (MirTypeId::Class(id), Type::Class(destination_id)) if id == destination_id => {
            types.class_fields(id)
        }
        _ => {
            return Err(Diagnostic::codegen(format!(
                "MIR constructs a non-struct/class value in function '{}'",
                function.name
            )))
        }
    };
    if fields.len() != declared.len() {
        return Err(Diagnostic::codegen(format!(
            "MIR constructor type {:?} has {} fields, expected {} in function '{}'",
            type_id,
            fields.len(),
            declared.len(),
            function.name,
        )));
    }
    for (field, (_, expected_type)) in fields.iter().zip(declared) {
        check_definition(available, *field, function)?;
        check_value_type(function, *field, *expected_type)?;
        check_owned_argument(function, *field, *expected_type, types, "constructor")?;
    }
    Ok(())
}
