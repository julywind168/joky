//! Stable, bounded contracts for independently compiled Script entries.
use crate::Type;

pub(crate) const MAX_LANES: usize = 64;
const MAX_DEPTH: usize = 8;

pub(crate) fn schema(ty: &Type) -> Option<String> {
    let mut fields_left = MAX_LANES;
    record_schema(ty, 0, &mut fields_left)
}
fn record_schema(ty: &Type, depth: usize, fields_left: &mut usize) -> Option<String> {
    if super::instance::scalar(ty) {
        return Some(format!("scalar:{ty}"));
    }
    let Type::Record(record) = ty else {
        return None;
    };
    if depth == MAX_DEPTH
        || record.closure.is_some()
        || record.template.is_some()
        || !record.arguments.is_empty()
        || !record.parameters.is_empty()
        || record.indirect
        || ty.is_owner()
        || record.fields.len() > *fields_left
    {
        return None;
    }
    // Count every field, including empty records and Unit, so zero-lane trees
    // cannot bypass the bound. Scalar slots can never exceed this field count.
    *fields_left -= record.fields.len();
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
            let nested = record_schema(&field.ty, depth + 1, fields_left)?;
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
