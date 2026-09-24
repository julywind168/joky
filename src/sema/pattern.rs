//! Pattern coverage and reachability analysis

use super::Type;

pub(super) const OPTION_PATTERN_ID_BASE: usize = usize::MAX / 2;
pub(super) const RESULT_PATTERN_ID_BASE: usize = usize::MAX / 4;

pub(super) const fn option_pattern_id(option_id: usize) -> usize {
    OPTION_PATTERN_ID_BASE + option_id
}

pub(super) const fn option_id_from_pattern_id(id: usize) -> Option<usize> {
    if id >= OPTION_PATTERN_ID_BASE {
        Some(id - OPTION_PATTERN_ID_BASE)
    } else {
        None
    }
}

pub(super) const fn result_pattern_id(result_id: usize) -> usize {
    RESULT_PATTERN_ID_BASE + result_id
}

pub(super) const fn result_id_from_pattern_id(id: usize) -> Option<usize> {
    if id >= RESULT_PATTERN_ID_BASE && id < OPTION_PATTERN_ID_BASE {
        Some(id - RESULT_PATTERN_ID_BASE)
    } else {
        None
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(super) enum CheckedPattern {
    Wildcard,
    EnumVariant {
        enum_id: usize,
        variant_index: usize,
        fields: Vec<CheckedPattern>,
    },
    Tuple {
        elements: Vec<CheckedPattern>,
    },
}

pub(super) trait PatternTypes {
    fn enum_variant_count(&self, enum_id: usize) -> usize;
    fn enum_variant_fields(&self, enum_id: usize, variant_index: usize) -> Vec<Type>;
    fn tuple_elements(&self, tuple_id: usize) -> Vec<Type>;
}

pub(super) fn is_useful(
    context: &impl PatternTypes,
    matrix: &[Vec<CheckedPattern>],
    patterns: &[CheckedPattern],
    types: &[Type],
) -> bool {
    let Some((pattern, remaining_patterns)) = patterns.split_first() else {
        return matrix.is_empty();
    };
    let (value_type, remaining_types) = types
        .split_first()
        .expect("each checked pattern has a type");
    match pattern {
        CheckedPattern::EnumVariant {
            enum_id,
            variant_index,
            fields,
        } => {
            let specialized = specialize(matrix, *enum_id, *variant_index, fields.len());
            let mut specialized_patterns = fields.clone();
            specialized_patterns.extend_from_slice(remaining_patterns);
            let mut specialized_types = context.enum_variant_fields(*enum_id, *variant_index);
            specialized_types.extend_from_slice(remaining_types);
            is_useful(
                context,
                &specialized,
                &specialized_patterns,
                &specialized_types,
            )
        }
        CheckedPattern::Tuple { elements } => {
            let specialized = specialize_tuple(matrix, elements.len());
            let mut specialized_patterns = elements.clone();
            specialized_patterns.extend_from_slice(remaining_patterns);
            let mut specialized_types = match value_type {
                Type::Tuple(tuple_id) => context.tuple_elements(*tuple_id),
                _ => vec![Type::Unit; elements.len()],
            };
            specialized_types.extend_from_slice(remaining_types);
            is_useful(
                context,
                &specialized,
                &specialized_patterns,
                &specialized_types,
            )
        }
        CheckedPattern::Wildcard => {
            if let Type::Tuple(tuple_id) = value_type {
                let field_types = context.tuple_elements(*tuple_id);
                let specialized = specialize_tuple(matrix, field_types.len());
                let mut specialized_patterns = vec![CheckedPattern::Wildcard; field_types.len()];
                specialized_patterns.extend_from_slice(remaining_patterns);
                let mut specialized_types = field_types;
                specialized_types.extend_from_slice(remaining_types);
                return is_useful(
                    context,
                    &specialized,
                    &specialized_patterns,
                    &specialized_types,
                );
            }
            let enum_id = match value_type {
                Type::Enum(enum_id) => *enum_id,
                Type::Option(option_id) => option_pattern_id(*option_id),
                Type::Result(result_id) => result_pattern_id(*result_id),
                _ => {
                    return is_useful(
                        context,
                        &default_matrix(matrix),
                        remaining_patterns,
                        remaining_types,
                    )
                }
            };
            let variant_count = context.enum_variant_count(enum_id);
            let mut present = vec![false; variant_count];
            for row in matrix {
                if let Some(CheckedPattern::EnumVariant {
                    enum_id: row_enum,
                    variant_index,
                    ..
                }) = row.first()
                {
                    if *row_enum == enum_id {
                        present[*variant_index] = true;
                    }
                }
            }
            if present.into_iter().all(|value| value) {
                return (0..variant_count).any(|variant_index| {
                    let field_types = context.enum_variant_fields(enum_id, variant_index);
                    let specialized = specialize(matrix, enum_id, variant_index, field_types.len());
                    let mut specialized_patterns =
                        vec![CheckedPattern::Wildcard; field_types.len()];
                    specialized_patterns.extend_from_slice(remaining_patterns);
                    let mut specialized_types = field_types;
                    specialized_types.extend_from_slice(remaining_types);
                    is_useful(
                        context,
                        &specialized,
                        &specialized_patterns,
                        &specialized_types,
                    )
                });
            }
            is_useful(
                context,
                &default_matrix(matrix),
                remaining_patterns,
                remaining_types,
            )
        }
    }
}

fn specialize(
    matrix: &[Vec<CheckedPattern>],
    enum_id: usize,
    variant_index: usize,
    field_count: usize,
) -> Vec<Vec<CheckedPattern>> {
    matrix
        .iter()
        .filter_map(|row| {
            let (head, tail) = row.split_first()?;
            let mut specialized = match head {
                CheckedPattern::Wildcard => vec![CheckedPattern::Wildcard; field_count],
                CheckedPattern::EnumVariant {
                    enum_id: row_enum,
                    variant_index: row_variant,
                    fields,
                } if *row_enum == enum_id && *row_variant == variant_index => fields.clone(),
                CheckedPattern::EnumVariant { .. } | CheckedPattern::Tuple { .. } => return None,
            };
            specialized.extend_from_slice(tail);
            Some(specialized)
        })
        .collect()
}

fn specialize_tuple(
    matrix: &[Vec<CheckedPattern>],
    field_count: usize,
) -> Vec<Vec<CheckedPattern>> {
    matrix
        .iter()
        .filter_map(|row| {
            let (head, tail) = row.split_first()?;
            let mut specialized = match head {
                CheckedPattern::Wildcard => vec![CheckedPattern::Wildcard; field_count],
                CheckedPattern::Tuple { elements } if elements.len() == field_count => {
                    elements.clone()
                }
                CheckedPattern::Tuple { .. } | CheckedPattern::EnumVariant { .. } => return None,
            };
            specialized.extend_from_slice(tail);
            Some(specialized)
        })
        .collect()
}

fn default_matrix(matrix: &[Vec<CheckedPattern>]) -> Vec<Vec<CheckedPattern>> {
    matrix
        .iter()
        .filter_map(|row| {
            let (head, tail) = row.split_first()?;
            matches!(head, CheckedPattern::Wildcard).then(|| tail.to_vec())
        })
        .collect()
}
