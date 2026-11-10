//! Ownership cleanup is explicit IR; source-visible borrowing remains checked
//! before backend cleanup registration is lowered.
use super::*;

#[derive(Clone)]
pub(crate) struct ElementCleanup {
    pub buffer: usize,
    pub owners: Vec<usize>,
}

pub(crate) enum CleanupOp {
    Register {
        value: Slot,
        place: Option<Slot>,
        elements: Vec<ElementCleanup>,
    },
}

impl Op {
    /// Include registrations performed when individual structured children finish.
    pub(crate) fn cleanups(&self) -> impl Iterator<Item = &CleanupOp> {
        let (own, branches) = match self {
            Op::Cleanup(cleanup) => (Some(cleanup), &[][..]),
            Op::Concurrent { branches, .. } => (None, branches.as_slice()),
            _ => (None, &[][..]),
        };
        own.into_iter()
            .chain(branches.iter().filter_map(|b| b.cleanup.as_ref()))
    }

    fn cleanups_mut(&mut self) -> impl Iterator<Item = &mut CleanupOp> {
        let (own, branches) = match self {
            Op::Cleanup(cleanup) => (Some(cleanup), &mut [][..]),
            Op::Concurrent { branches, .. } => (None, branches.as_mut_slice()),
            _ => (None, &mut [][..]),
        };
        own.into_iter()
            .chain(branches.iter_mut().filter_map(|b| b.cleanup.as_mut()))
    }
}

impl FunctionPlan {
    /// Prove the complete storage graph, including definitions behind buffers.
    /// Bound expansion also rejects recursive graphs, whose metadata cannot yet
    /// be represented by the domain ABI. No user callback may outlive a turn.
    pub(crate) fn domain_storage(&self, root: &Type) -> bool {
        let mut pending = vec![(root.clone(), 0)];
        let mut visited = 0;
        while let Some((ty, depth)) = pending.pop() {
            visited += 1;
            if depth > 32 || visited > 65_536 {
                return false;
            }
            let Ok(ty) = self.pointee(&ty, &self.type_registry) else {
                return false;
            };
            if matches!(ty, Type::Enum(ref e) if e.compact) || ty.storage_layout().is_err() {
                return false;
            }
            let children: Vec<Type> = match &ty {
                Type::Record(r) if r.closure.is_none() && !r.indirect => r
                    .arguments
                    .iter()
                    .filter_map(|a| a.as_type())
                    .cloned()
                    .chain(r.fields.iter().map(|f| f.ty.clone()))
                    .collect(),
                Type::Buffer(element) => vec![(**element).clone()],
                Type::Tuple(t) => t.elements.clone(),
                Type::Enum(e) if !e.indirect => e
                    .arguments
                    .iter()
                    .filter_map(|a| a.as_type())
                    .cloned()
                    .chain(e.variants.iter().filter_map(|v| v.payload.clone()))
                    .collect(),
                _ if crate::core::instance::scalar(&ty) || ty == Type::String => Vec::new(),
                _ => return false,
            };
            pending.extend(children.into_iter().map(|ty| (ty, depth + 1)));
        }
        true
    }

    pub(crate) fn has_cleanups(&self) -> bool {
        self.ops.iter().any(|op| {
            op.cleanups().next().is_some()
                || match op {
                    Op::Buffer(BufferOp::SwapReferences { target, .. }) if target.is_place() => {
                        true
                    }
                    Op::Buffer(BufferOp::Move { .. } | BufferOp::End(_)) | Op::Place(_) => true,
                    Op::Buffer(BufferOp::Storage { dst: Some(dst), .. }) => dst.ty.is_owner_lane(),
                    _ => false,
                }
        })
    }

    /// Value transport never retains or invokes user-defined destructor code.
    pub(crate) fn domain_value(&self, ty: &Type) -> bool {
        crate::core::script::domain_schema(ty).is_some()
    }

    pub(super) fn register_owner(&mut self, value: Slot, place: Option<Slot>) {
        if value.ty.is_place() {
            self.emit(Op::Place(PlaceOp::Copy {
                dst: value.clone(),
                src: value,
            }));
            return;
        }
        self.ops.push(
            Op::Cleanup(CleanupOp::Register {
                value,
                place,
                elements: vec![],
            }),
            self.current_span,
        );
    }

    pub(super) fn resolve_cleanups(&mut self) -> Result<(), Error> {
        let mut pending = Vec::new();
        for (index, op) in self.ops.iter().enumerate() {
            for (cleanup_index, CleanupOp::Register { value, .. }) in op.cleanups().enumerate() {
                pending.push((index, cleanup_index, value.ty.clone()));
            }
        }
        for (index, cleanup_index, ty) in pending {
            let span = self.ops.spans()[index];
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
                    } else if element.is_place() {
                        element.place_owner_offsets()
                    } else {
                        element
                            .storage_scalars()
                            .map_err(|error| {
                                type_error(&span, ErrorCode::ResourceLimit, error.to_string())
                            })?
                            .into_iter()
                            .filter(|(_, ty)| ty.is_owner_lane())
                            .map(|(offset, _)| offset)
                            .collect()
                    },
                });
            }
            let CleanupOp::Register {
                elements: target_elements,
                ..
            } = self.ops[index].cleanups_mut().nth(cleanup_index).unwrap();
            *target_elements = elements;
        }
        Ok(())
    }
}
