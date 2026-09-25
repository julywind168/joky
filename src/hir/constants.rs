use super::{CoreExpr, CoreExprKind};
use crate::module::constants::{ConstantKind, ConstantValue};
use crate::syntax::NodeId;

pub(super) fn lower_imported_constant(
    value: &ConstantValue,
    id: NodeId,
    types: &crate::sema::CheckedTypes,
) -> CoreExpr {
    let kind = match &value.kind {
        ConstantKind::Integer(v) => CoreExprKind::Integer(*v),
        ConstantKind::Float(v) => CoreExprKind::Float(*v),
        ConstantKind::Duration(v) => CoreExprKind::Duration(*v),
        ConstantKind::String(v) => CoreExprKind::String(v.clone()),
        ConstantKind::Bytes(v) => CoreExprKind::EmbeddedBytes(v.clone()),
        ConstantKind::Boolean(v) => CoreExprKind::Boolean(*v),
        ConstantKind::UnitVariant(name) => {
            let crate::sema::Type::Enum(enum_id) = value.ty else {
                unreachable!()
            };
            CoreExprKind::Field {
                value: Box::new(CoreExpr {
                    id,
                    ty: value.ty,
                    kind: CoreExprKind::Name(types.enum_name(enum_id).into()),
                }),
                access: crate::syntax::FieldAccess::Name(name.clone()),
            }
        }
        ConstantKind::Unary(op, value) => CoreExprKind::Unary {
            op: *op,
            expression: Box::new(lower_imported_constant(value, id, types)),
        },
        ConstantKind::Binary(op, left, right) if op.is_comparison() => super::lower_comparison(
            id,
            *op,
            lower_imported_constant(left, id, types),
            lower_imported_constant(right, id, types),
            types,
        ),
        ConstantKind::Binary(op, left, right) => CoreExprKind::Binary {
            op: *op,
            left: Box::new(lower_imported_constant(left, id, types)),
            right: Box::new(lower_imported_constant(right, id, types)),
        },
        ConstantKind::Tuple(elements) => CoreExprKind::Tuple(
            elements
                .iter()
                .map(|v| lower_imported_constant(v, id, types))
                .collect(),
        ),
        ConstantKind::Struct(fields) => {
            let crate::sema::Type::Struct(struct_id) = value.ty else {
                unreachable!("typed struct default")
            };
            CoreExprKind::StructInit {
                name: types.struct_name(struct_id).into(),
                fields: fields
                    .iter()
                    .map(|(name, value)| (name.clone(), lower_imported_constant(value, id, types)))
                    .collect(),
            }
        }
    };
    CoreExpr {
        id,
        ty: value.ty,
        kind,
    }
}
