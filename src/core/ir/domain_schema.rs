//! Freeze finite domain storage definitions while the compiler still owns nominals.
use crate::{core::ir::FunctionPlan, parser::ast::*, Error};
use std::sync::Arc;

pub(super) fn freeze(body: &FunctionPlan, ty: &Type) -> Result<Type, Error> {
    fn visit(
        body: &FunctionPlan,
        ty: &Type,
        depth: usize,
        left: &mut usize,
    ) -> Result<Type, Error> {
        if depth > 64 || *left == 0 {
            return Err(Error::new_with_code(
                crate::ErrorPhase::TypeCheck,
                crate::ErrorCode::TypeMismatch,
                "Domain schema exceeds finite storage limits",
            ));
        }
        *left -= 1;
        let ty = body.pointee(ty, &body.type_registry)?;
        let mut child = |ty: &Type| visit(body, ty, depth + 1, left);
        Ok(match &ty {
            Type::Reference(r) => Type::reference(child(&r.target)?, r.mutable),
            Type::Buffer(t) => Type::Buffer(Arc::new(child(t)?)),
            Type::Record(r) => {
                let fields = r
                    .fields
                    .iter()
                    .map(|f| {
                        Ok(RecordField {
                            name: f.name.clone(),
                            private: f.private,
                            offset: f.offset,
                            ty: child(&f.ty)?,
                        })
                    })
                    .collect::<Result<Vec<_>, Error>>()?;
                Type::Record(Arc::new(RecordType {
                    array: false,
                    closure: None,
                    owners: fields
                        .iter()
                        .flat_map(|f| f.ty.buffer_elements().iter().cloned())
                        .collect(),
                    repr_c: r.repr_c,
                    indirect: false,
                    id: r.id,
                    name: r.name.clone(),
                    parameters: r.parameters.clone(),
                    arguments: r
                        .arguments
                        .iter()
                        .map(|a| a.try_map_type(&mut child))
                        .collect::<Result<_, _>>()?,
                    template: r.template.clone(),
                    scalars: fields
                        .iter()
                        .flat_map(|f| f.ty.scalar_types().iter().cloned())
                        .collect(),
                    depth: r.depth,
                    fields,
                }))
            }
            Type::Tuple(t) => {
                let elements = t
                    .elements
                    .iter()
                    .map(&mut child)
                    .collect::<Result<Vec<_>, _>>()?;
                Type::Tuple(Arc::new(TupleType {
                    owners: elements
                        .iter()
                        .flat_map(|t| t.buffer_elements().iter().cloned())
                        .collect(),
                    scalars: elements
                        .iter()
                        .flat_map(|t| t.scalar_types().iter().cloned())
                        .collect(),
                    depth: t.depth,
                    elements,
                }))
            }
            Type::Enum(e) => {
                let variants = e
                    .variants
                    .iter()
                    .map(|v| {
                        Ok(EnumVariant {
                            name: v.name.clone(),
                            offset: v.offset,
                            payload: v.payload.as_ref().map(&mut child).transpose()?,
                        })
                    })
                    .collect::<Result<Vec<_>, Error>>()?;
                Type::Enum(Arc::new(EnumType {
                    owners: Vec::new(),
                    indirect: false,
                    id: e.id,
                    name: e.name.clone(),
                    parameters: e.parameters.clone(),
                    arguments: e
                        .arguments
                        .iter()
                        .map(|a| a.try_map_type(&mut child))
                        .collect::<Result<_, _>>()?,
                    template: e.template.clone(),
                    variants,
                    scalars: e.scalars.clone(),
                    depth: e.depth,
                }))
            }
            _ => ty,
        })
    }
    visit(body, ty, 0, &mut 65_536)
}
