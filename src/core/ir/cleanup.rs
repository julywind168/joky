//! Ownership cleanup is explicit IR; source-visible borrowing remains checked
//! before backend cleanup registration is lowered.
use super::*;

#[derive(Clone)]
pub(crate) struct DropHook {
    pub name: String,
    pub offset: usize,
    pub indirect: bool,
}

pub(crate) enum CleanupOp {
    Register {
        value: Slot,
        place: Option<Slot>,
        hooks: Vec<DropHook>,
    },
}

impl FunctionPlan {
    pub(crate) fn has_cleanups(&self) -> bool {
        self.ops.iter().any(|op| match op {
            Op::Cleanup(_) | Op::Buffer(BufferOp::Move { .. } | BufferOp::End(_)) => true,
            Op::Buffer(BufferOp::Storage { dst: Some(dst), .. }) => dst.ty.is_buffer(),
            _ => false,
        })
    }

    pub(super) fn check_drop_call(&self, name: &str, span: Span) -> Result<(), Error> {
        if self
            .generics
            .borrow()
            .members
            .traits
            .definitions
            .iter()
            .any(|d| {
                d.is_drop
                    && (name.contains(&format!("joky/impl/{}/", d.self_parameter.id))
                        || name.contains(&format!("joky/trait/{}/", d.self_parameter.id)))
            })
        {
            return Err(type_error(
                &span,
                ErrorCode::InvalidCall,
                "Drop hooks cannot be called explicitly",
            ));
        }
        Ok(())
    }

    pub(super) fn has_custom_drop(&self, ty: &Type) -> bool {
        let context = self.generics.borrow();
        context.members.methods.iter().any(|method| {
            method.trait_id.is_some_and(|id| {
                context
                    .members
                    .traits
                    .definitions
                    .iter()
                    .any(|definition| definition.is_drop && definition.self_parameter.id == id)
            }) && members::same_owner(&method.target, ty)
        })
    }

    pub(super) fn check_owner_projection(
        &self,
        mut ty: Type,
        path: &[String],
        span: Span,
    ) -> Result<(), Error> {
        for name in path {
            if self.has_custom_drop(&ty) {
                return Err(type_error(
                    &span,
                    ErrorCode::InvalidAssignment,
                    "Cannot move or replace an owning field of a record implementing Drop",
                ));
            }
            let Type::Record(record) = ty else { break };
            let Some(field) = record.fields.iter().find(|f| &f.name == name) else {
                break;
            };
            ty = field.ty.clone();
        }
        Ok(())
    }

    pub(super) fn register_owner(&mut self, value: Slot, place: Option<Slot>) {
        self.ops.push(Op::Cleanup(CleanupOp::Register {
            value,
            place,
            hooks: vec![],
        }));
        self.op_spans.push(self.current_span);
    }

    pub(super) fn resolve_cleanups(&mut self) -> Result<(), Error> {
        let mut pending = Vec::new();
        for (index, op) in self.ops.iter().enumerate() {
            if let Op::Cleanup(CleanupOp::Register { value, .. }) = op {
                pending.push((index, value.ty.clone()));
            }
        }
        for (index, mut ty) in pending {
            let mut hooks = Vec::new();
            let mut offset = 0;
            while let Type::Record(record) = &ty {
                let context = self.generics.borrow();
                let method = context.members.methods.iter().find(|method| {
                    method.trait_id.is_some_and(|id| {
                        context
                            .members
                            .traits
                            .definitions
                            .iter()
                            .any(|d| d.is_drop && d.self_parameter.id == id)
                    }) && members::same_owner(&method.target, &ty)
                });
                if let Some(method) = method {
                    let mut name = method.function.clone();
                    if let Some(template) = context.templates.get(&name) {
                        let mut substitutions = HashMap::new();
                        generics::unify(
                            &method.target,
                            &ty,
                            &template.type_params,
                            &mut substitutions,
                            self.op_spans[index],
                        )?;
                        let arguments = template
                            .type_params
                            .iter()
                            .map(|p| substitutions[&p.id].clone())
                            .collect();
                        drop(context);
                        name = self.generics.borrow_mut().instantiate(
                            &name,
                            arguments,
                            crate::FunctionSignature::new(
                                vec![Type::reference(ty.clone(), true)],
                                Type::Unit,
                            ),
                            self.instantiation_depth + 1,
                            self.op_spans[index],
                        )?;
                    } else {
                        drop(context);
                    }
                    hooks.push(DropHook {
                        name,
                        offset,
                        indirect: !ty.is_inline_owner(),
                    });
                } else {
                    drop(context);
                }
                let field = record.fields.iter().find(|f| f.ty.is_owner()).unwrap();
                // Rigid templates are checked but never lowered.
                if !self.generics.borrow().checking {
                    offset += ty
                        .storage_field_path(std::slice::from_ref(&field.name))
                        .map_err(|e| {
                            type_error(
                                &self.op_spans[index],
                                ErrorCode::ResourceLimit,
                                e.to_string(),
                            )
                        })?
                        .0;
                }
                ty = field.ty.clone();
            }
            let Op::Cleanup(CleanupOp::Register { hooks: target, .. }) = &mut self.ops[index]
            else {
                unreachable!()
            };
            *target = hooks;
        }
        Ok(())
    }
}
