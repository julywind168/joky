//! Stable, bounded contracts for independently compiled Script entries.
use crate::Type;

pub(crate) const MAX_LANES: usize = 64;
const MAX_DEPTH: usize = 8;
const MAX_MEMBERS: usize = 64;

pub(crate) fn schema(ty: &Type) -> Option<String> {
    let mut members_left = MAX_MEMBERS;
    value_schema(ty, 0, &mut members_left)
}
fn value_schema(ty: &Type, depth: usize, members_left: &mut usize) -> Option<String> {
    if super::instance::scalar(ty) {
        return Some(format!("scalar:{ty}"));
    }
    if depth == MAX_DEPTH || ty.is_owner() || ty.scalar_types().len() > MAX_LANES {
        return None;
    }
    if let Type::Enum(enumeration) = ty {
        if enumeration.indirect || enumeration.variants.is_empty() {
            return None;
        }
        // Bound the complete contract, including absent payloads and phantom
        // arguments. Concrete generic enums are accepted; rigid parameters are not.
        let members = 1 + enumeration.arguments.len() + enumeration.variants.len();
        *members_left = members_left.checked_sub(members)?;
        let mut result = format!("enum:{}:{}", enumeration.name.len(), enumeration.name);
        result.push_str(&format!(";args:{}", enumeration.arguments.len()));
        for argument in &enumeration.arguments {
            let contract = value_schema(argument, depth + 1, members_left)?;
            result.push_str(&format!(";{}:{contract}", contract.len()));
        }
        result.push_str(&format!(";variants:{}", enumeration.variants.len()));
        for variant in &enumeration.variants {
            result.push_str(&format!(";{}:{}", variant.name.len(), variant.name));
            match &variant.payload {
                None => result.push_str(":none"),
                Some(payload) => {
                    let contract = value_schema(payload, depth + 1, members_left)?;
                    result.push_str(&format!(":some:{}:{contract}", contract.len()));
                }
            }
        }
        return Some(result);
    }
    let Type::Record(record) = ty else {
        return None;
    };
    if record.closure.is_some()
        || record.template.is_some()
        || !record.arguments.is_empty()
        || !record.parameters.is_empty()
        || record.indirect
        || record.fields.len() > *members_left
    {
        return None;
    }
    // Count every field, including empty records and Unit, so zero-lane trees
    // cannot bypass the bound. Scalar slots can never exceed this field count.
    *members_left -= record.fields.len();
    let mut result = format!(
        "record:{}:{}:{}",
        record.name.len(),
        record.name,
        record.repr_c
    );
    for field in &record.fields {
        let contract = if super::instance::scalar(&field.ty) {
            // Preserve the already published encoding of flat contracts.
            field.ty.to_string()
        } else {
            let nested = value_schema(&field.ty, depth + 1, members_left)?;
            format!("nested:{}:{nested}", nested.len())
        };
        result.push_str(&format!(
            ";{}:{}:{}:{}",
            field.name.len(),
            field.name,
            field.private,
            contract
        ));
    }
    Some(result)
}

/// Validate complete packet values, including tags and canonical inactive lanes.
/// The type must already have passed schema validation. A zeroed inactive
/// nested enum is not an active value and must not be decoded.
pub(crate) fn valid_words(ty: &Type, words: &[u64]) -> bool {
    fn valid(ty: &Type, words: &[u64], depth: usize) -> bool {
        if words.len() != ty.scalar_types().len() || depth > MAX_DEPTH {
            return false;
        }
        match ty {
            Type::Record(record) => record.fields.iter().all(|field| {
                valid(
                    &field.ty,
                    &words[field.offset..field.offset + field.ty.scalar_types().len()],
                    depth + 1,
                )
            }),
            Type::Enum(enumeration) => {
                let Some(&tag) = words.first() else {
                    return false;
                };
                let Some(variant) = usize::try_from(tag)
                    .ok()
                    .and_then(|tag| enumeration.variants.get(tag))
                else {
                    return false;
                };
                let start = variant.offset;
                let end = start
                    + variant
                        .payload
                        .as_ref()
                        .map_or(0, |ty| ty.scalar_types().len());
                words[1..start]
                    .iter()
                    .chain(&words[end..])
                    .all(|word| *word == 0)
                    && variant
                        .payload
                        .as_ref()
                        .is_none_or(|ty| valid(ty, &words[start..end], depth + 1))
            }
            Type::Unit => words.is_empty(),
            _ => {
                words.len() == 1
                    && super::instance::InstanceValue::from_bits(ty, words[0]).is_some()
            }
        }
    }
    words.len() <= MAX_LANES && valid(ty, words, 0)
}

#[cfg(test)]
mod tests;
