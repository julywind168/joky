//! Ownership cleanup is explicit IR; source-visible borrowing remains checked
//! before backend cleanup registration is lowered.
use super::*;

#[derive(Clone)]
pub(crate) struct DropHook {
    pub name: String,
    /// Run immediately before releasing this buffer in declaration order.
    pub before: usize,
    pub offset: usize,
    pub indirect: bool,
    /// Enclosing enum tag byte offsets and their active variant indices.
    pub guards: Vec<(usize, i32)>,
}

#[derive(Clone)]
pub(crate) struct ElementCleanup {
    pub buffer: usize,
    pub owners: Vec<usize>,
    pub hooks: Vec<DropHook>,
}

pub(crate) enum CleanupOp {
    Register {
        value: Slot,
        place: Option<Slot>,
        hooks: Vec<DropHook>,
        elements: Vec<ElementCleanup>,
    },
}

impl FunctionPlan {
    pub(crate) fn domain_array_identity(&self) -> Option<u64> {
        self.type_registry
            .standard
            .get()
            .and_then(|library| library.array_identity())
    }

    /// Root hooks use the same specialized descriptors as ordinary local cleanup.
    pub(crate) fn instance_hooks(&self, ty: &Type) -> Vec<DropHook> {
        self.ops
            .iter()
            .find_map(|op| match op {
                Op::Cleanup(CleanupOp::Register { value, hooks, .. }) if &value.ty == ty => {
                    Some(hooks.clone())
                }
                _ => None,
            })
            .unwrap_or_default()
    }

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

    pub(crate) fn has_custom_drop(&self, ty: &Type) -> bool {
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
            elements: vec![],
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
        for (index, ty) in pending {
            let span = self.op_spans[index];
            let hooks = self.owner_hooks(ty.clone(), span)?;
            let mut elements = Vec::new();
            for (buffer, scalar) in ty.scalar_types().iter().enumerate() {
                let Type::Buffer(element) = scalar else {
                    continue;
                };
                let element = self.pointee(element, &self.type_registry)?;
                if !element.is_owner() {
                    continue;
                }
                elements.push(ElementCleanup {
                    buffer,
                    owners: if self.generics.borrow().checking {
                        Vec::new()
                    } else {
                        element
                            .storage_scalars()
                            .map_err(|error| {
                                type_error(&span, ErrorCode::ResourceLimit, error.to_string())
                            })?
                            .into_iter()
                            .filter(|(_, ty)| ty.is_buffer())
                            .map(|(offset, _)| offset)
                            .collect()
                    },
                    hooks: self.owner_hooks(element.clone(), span)?,
                });
            }
            let Op::Cleanup(CleanupOp::Register {
                hooks: target,
                elements: target_elements,
                ..
            }) = &mut self.ops[index]
            else {
                unreachable!()
            };
            *target = hooks;
            *target_elements = elements;
        }
        Ok(())
    }
    fn owner_hooks(&mut self, ty: Type, span: Span) -> Result<Vec<DropHook>, Error> {
        let mut hooks = Vec::new();
        let mut before = 0;
        let mut work = vec![(ty, 0, Vec::new())];
        while let Some((ty, offset, guards)) = work.pop() {
            if ty.is_buffer() {
                before += 1;
                continue;
            }
            if let Type::Enum(enumeration) = &ty {
                let locations = if self.generics.borrow().checking {
                    vec![0; enumeration.variants.len()]
                } else {
                    let crate::memory_layout::MemoryLayoutShape::Enum { variants, .. } = ty
                        .storage_layout()
                        .map_err(|e| type_error(&span, ErrorCode::ResourceLimit, e.to_string()))?
                        .shape
                    else {
                        unreachable!()
                    };
                    variants
                        .iter()
                        .map(|v| v.payload.map_or(0, |p| p.offset))
                        .collect()
                };
                for (index, variant) in enumeration.variants.iter().enumerate().rev() {
                    if let Some(payload) = variant.payload.as_ref().filter(|ty| ty.is_owner()) {
                        let mut nested = guards.clone();
                        nested.push((offset, index as i32));
                        work.push((payload.clone(), offset + locations[index], nested));
                    }
                }
                continue;
            }
            if let Type::Tuple(tuple) = &ty {
                let locations = if self.generics.borrow().checking {
                    vec![0; tuple.elements.len()]
                } else {
                    let crate::memory_layout::MemoryLayoutShape::Tuple(fields) = ty
                        .storage_layout()
                        .map_err(|e| type_error(&span, ErrorCode::ResourceLimit, e.to_string()))?
                        .shape
                    else {
                        unreachable!()
                    };
                    fields.iter().map(|field| field.offset).collect()
                };
                for (element, location) in tuple.elements.iter().zip(locations).rev() {
                    if element.is_owner() {
                        work.push((element.clone(), offset + location, guards.clone()));
                    }
                }
                continue;
            }
            let Type::Record(record) = &ty else {
                unreachable!()
            };
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
                        span,
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
                        span,
                    )?;
                } else {
                    drop(context);
                }
                hooks.push(DropHook {
                    name,
                    before,
                    offset,
                    indirect: !ty.is_inline_owner(),
                    guards: guards.clone(),
                });
            } else {
                drop(context);
            }
            for field in record.fields.iter().rev().filter(|f| f.ty.is_owner()) {
                // Rigid templates are checked but never lowered.
                let field_offset = if self.generics.borrow().checking {
                    0
                } else {
                    ty.storage_field_path(std::slice::from_ref(&field.name))
                        .map_err(|e| type_error(&span, ErrorCode::ResourceLimit, e.to_string()))?
                        .0
                };
                work.push((field.ty.clone(), offset + field_offset, guards.clone()));
            }
        }
        Ok(hooks)
    }
}
