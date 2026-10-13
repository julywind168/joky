//! Stable, bounded contracts for independently compiled Script entries.
use crate::Type;

pub(crate) const MAX_LANES: usize = 64;

pub(crate) fn schema(ty: &Type) -> Option<String> {
    if super::instance::scalar(ty) {
        return Some(format!("scalar:{ty}"));
    }
    let Type::Record(record) = ty else {
        return None;
    };
    if record.closure.is_some()
        || record.template.is_some()
        || !record.arguments.is_empty()
        || !record.parameters.is_empty()
        || record.indirect
        || ty.is_owner()
        || record.fields.len() > MAX_LANES
        || !record.fields.iter().all(|f| super::instance::scalar(&f.ty))
    {
        return None;
    }
    let mut result = format!(
        "record:{}:{}:{}",
        record.name.len(),
        record.name,
        record.repr_c
    );
    for field in &record.fields {
        result.push_str(&format!(
            ";{}:{}:{}:{}",
            field.name.len(),
            field.name,
            field.private,
            field.ty
        ));
    }
    Some(result)
}
