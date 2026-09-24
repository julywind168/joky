use super::*;

type EnumVariantFields = [(String, Type)];
type ResolvedPatternVariant = (MirTypeId, usize, Vec<(String, Type)>);

pub(super) fn resolved_pattern_variant(
    enum_name: &str,
    variant: &str,
    value_type: Type,
    types: &CheckedTypes,
) -> Result<ResolvedPatternVariant, Diagnostic> {
    if enum_name == "Option" || matches!(value_type, Type::Option(_)) {
        let Type::Option(option_id) = value_type else {
            return Err(Diagnostic::codegen("Option pattern value was not resolved"));
        };
        let variant_index = usize::from(variant == "None");
        if !matches!(variant, "Some" | "None") {
            return Err(Diagnostic::codegen(
                "Option pattern variant was not resolved",
            ));
        }
        return Ok((
            MirTypeId::Option(option_id),
            variant_index,
            (variant_index == 0)
                .then_some(("value".to_owned(), types.option_type(option_id)))
                .into_iter()
                .collect(),
        ));
    }
    if enum_name == "Result" || matches!(value_type, Type::Result(_)) {
        let Type::Result(result_id) = value_type else {
            return Err(Diagnostic::codegen("Result pattern value was not resolved"));
        };
        let variant_index = usize::from(variant == "Err");
        if !matches!(variant, "Ok" | "Err") {
            return Err(Diagnostic::codegen(
                "Result pattern variant was not resolved",
            ));
        }
        let (ok, err) = types.result_types(result_id);
        return Ok((
            MirTypeId::Result(result_id),
            variant_index,
            vec![(
                "value".to_owned(),
                if variant_index == 0 { ok } else { err },
            )],
        ));
    }
    let enum_id = match types.enum_type(enum_name) {
        Some(Type::Enum(id)) => id,
        _ => match value_type {
            Type::Enum(id) => id,
            _ => return Err(Diagnostic::codegen("enum pattern type was not resolved")),
        },
    };
    let variant_index = types
        .enum_variants(enum_id)
        .iter()
        .position(|candidate| candidate.name == variant)
        .ok_or_else(|| Diagnostic::codegen("enum pattern variant was not resolved"))?;
    Ok((
        MirTypeId::Enum(enum_id),
        variant_index,
        types.enum_variants(enum_id)[variant_index].fields.clone(),
    ))
}

pub(super) fn ordered_pattern_fields<'a>(
    fields: &'a [CorePatternField],
    declared: &EnumVariantFields,
) -> Result<Vec<&'a CorePattern>, Diagnostic> {
    let mut ordered = vec![None; declared.len()];
    let mut next_positional = 0;
    for field in fields {
        let shorthand = match &field.pattern {
            CorePattern::Binding { name, .. } if field.label.is_none() => Some(name),
            _ => None,
        };
        let index = if let Some(label) = field.label.as_ref().or(shorthand) {
            declared
                .iter()
                .position(|(name, _)| name == label)
                .ok_or_else(|| Diagnostic::codegen("pattern field was not resolved"))?
        } else {
            while next_positional < ordered.len() && ordered[next_positional].is_some() {
                next_positional += 1;
            }
            let index = next_positional;
            next_positional += 1;
            index
        };
        if index >= ordered.len() || ordered[index].is_some() {
            return Err(Diagnostic::codegen("pattern field was not resolved"));
        }
        ordered[index] = Some(&field.pattern);
    }
    ordered
        .into_iter()
        .map(|pattern| pattern.ok_or_else(|| Diagnostic::codegen("missing pattern field")))
        .collect()
}
