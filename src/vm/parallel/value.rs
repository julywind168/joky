use super::*;
use crate::runtime::{
    arena::Arena,
    parallel::packet::{Packet, Shape},
};

impl TaskValue {
    pub(super) fn matches(&self, ty: &Type) -> bool {
        match (self, ty) {
            (Self::Scalar(value), ty) => value.value_type() == *ty,
            (Self::String(_), Type::String) => true,
            (
                Self::FixedArray {
                    ty: actual,
                    elements,
                },
                _,
            ) if actual == ty => ty
                .fixed_array_element_type()
                .is_some_and(|(element, length)| {
                    elements.len() as u64 == length && elements.iter().all(|v| v.matches(element))
                }),
            (Self::Tuple(values), Type::Tuple(t)) => {
                values.len() == t.elements.len()
                    && values.iter().zip(&t.elements).all(|(v, t)| v.matches(t))
            }
            (
                Self::Array {
                    ty: actual,
                    elements,
                },
                _,
            ) if actual == ty => ty
                .array_element_type()
                .is_some_and(|element| elements.iter().all(|v| v.matches(element))),
            (
                Self::List {
                    ty: actual,
                    elements,
                },
                _,
            ) if actual == ty => ty
                .list_element_type()
                .is_some_and(|element| elements.iter().all(|v| v.matches(element))),
            (Self::Map(TaskMap { ty: actual, fields }), Type::Record(r)) if r.map => {
                actual == ty
                    && fields.len() == r.fields.len()
                    && fields.iter().zip(&r.fields).all(|(v, f)| v.matches(&f.ty))
            }
            (Self::Set(TaskSet { ty: actual, fields }), Type::Record(r)) if r.set => {
                actual == ty
                    && fields.len() == r.fields.len()
                    && fields.iter().zip(&r.fields).all(|(v, f)| v.matches(&f.ty))
            }
            (Self::Record { ty: actual, fields }, Type::Record(r))
                if !r.array && !r.list && !r.map && !r.set =>
            {
                actual == ty
                    && fields.len() == r.fields.len()
                    && fields.iter().zip(&r.fields).all(|(v, f)| v.matches(&f.ty))
            }
            (
                Self::Enum {
                    ty: actual,
                    variant,
                    payload,
                },
                Type::Enum(e),
            ) if actual == ty => {
                e.variants
                    .get(*variant)
                    .is_some_and(|v| match (payload, &v.payload) {
                        (None, None) => true,
                        (Some(value), Some(ty)) => value.matches(ty),
                        _ => false,
                    })
            }
            _ => false,
        }
    }
    pub(super) fn bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + match self {
                Self::String(text) => text.capacity(),
                Self::Tuple(v)
                | Self::Record { fields: v, .. }
                | Self::Map(TaskMap { fields: v, .. })
                | Self::Set(TaskSet { fields: v, .. })
                | Self::Array { elements: v, .. }
                | Self::List { elements: v, .. }
                | Self::FixedArray { elements: v, .. } => {
                    v.iter().map(Self::bytes).sum::<usize>()
                        + (v.capacity() - v.len()) * std::mem::size_of::<Self>()
                }
                Self::Enum { payload, .. } => payload.as_ref().map_or(0, |v| v.bytes()),
                _ => 0,
            }
    }
    pub(super) fn packet(&self, ty: &Type, arena: &mut Arena) -> Result<Packet, Error> {
        fn fill(
            value: &TaskValue,
            ty: &Type,
            packet: &mut Packet,
            base: usize,
            arena: &mut Arena,
        ) -> Result<(), Error> {
            match value {
                TaskValue::FixedArray { .. } => {
                    unreachable!("inline Copy values use typed storage")
                }
                TaskValue::Scalar(value) => {
                    if *ty != Type::Unit {
                        packet.words[base] = value.bits();
                    }
                }
                TaskValue::String(text) => packet.push_text(base, text, arena)?,
                TaskValue::Array { elements, .. } => {
                    let raw = arrays::input(ty, elements, arena)?;
                    unsafe { packet.push_buffer(base, raw, arena)? };
                }
                TaskValue::List { elements, .. } => {
                    lists::input(ty, elements, packet, base, arena)?;
                }
                TaskValue::Tuple(values) => {
                    let Type::Tuple(t) = ty else { unreachable!() };
                    let mut offset = base;
                    for (value, ty) in values.iter().zip(&t.elements) {
                        fill(value, ty, packet, offset, arena)?;
                        offset += ty.scalar_types().len();
                    }
                }
                TaskValue::Record { fields, .. }
                | TaskValue::Map(TaskMap { fields, .. })
                | TaskValue::Set(TaskSet { fields, .. }) => {
                    let Type::Record(r) = ty else { unreachable!() };
                    for (value, field) in fields.iter().zip(&r.fields) {
                        fill(value, &field.ty, packet, base + field.offset, arena)?;
                    }
                }
                TaskValue::Enum {
                    variant, payload, ..
                } => {
                    let Type::Enum(e) = ty else { unreachable!() };
                    packet.words[base] = *variant as u64;
                    if let Some(value) = payload {
                        let v = &e.variants[*variant];
                        fill(
                            value,
                            v.payload.as_ref().unwrap(),
                            packet,
                            base + v.offset,
                            arena,
                        )?;
                    }
                }
            }
            Ok(())
        }
        let mut packet = Packet::empty(Shape::of(ty)?)?;
        if ty.is_place() {
            unsafe {
                places::write(self, ty, packet.place_storage())?;
            }
        } else {
            fill(self, ty, &mut packet, 0, arena)?;
        }
        Ok(packet)
    }
}

pub(super) fn output(packet: &Packet, ty: &Type, root: &Storage) -> Result<ParallelOutput, Error> {
    fn measure(ty: &Type, words: &[u64]) -> Result<usize, Error> {
        if ty.is_place() {
            return unsafe { places::measure(ty, words[0] as *const u8) };
        }
        let children = match ty {
            Type::Record(r) if r.array => {
                let mut bytes = 0usize;
                unsafe {
                    arrays::visit(ty, words[0], true, |element, words| {
                        bytes = bytes
                            .checked_add(measure(element, words)?)
                            .ok_or_else(|| invalid("Parallel array result size overflow"))?;
                        Ok(())
                    })?;
                }
                bytes
            }
            Type::Record(r) if r.list => {
                let mut bytes = 0usize;
                unsafe {
                    lists::visit(ty, words, true, |element, words| {
                        bytes = bytes
                            .checked_add(measure(element, words)?)
                            .ok_or_else(|| invalid("Parallel list result size overflow"))?;
                        Ok(())
                    })?;
                }
                bytes
            }
            Type::String => {
                unsafe { crate::runtime::owned_text::borrow(words[0] as *const _) }?.len()
            }
            Type::Tuple(t) => {
                let mut base = 0;
                let mut bytes = 0;
                for ty in &t.elements {
                    let end = base + ty.scalar_types().len();
                    bytes += measure(ty, &words[base..end])?;
                    base = end;
                }
                bytes
            }
            Type::Record(r) => {
                if r.map {
                    unsafe {
                        maps::validate(ty, words)?;
                    }
                }
                let mut bytes = 0;
                for f in &r.fields {
                    bytes += measure(
                        &f.ty,
                        &words[f.offset..f.offset + f.ty.scalar_types().len()],
                    )?;
                }
                bytes
            }
            Type::Enum(e) => {
                let v = e
                    .variants
                    .get(words[0] as usize)
                    .ok_or_else(|| invalid("Invalid parallel enum result"))?;
                v.payload.as_ref().map_or(Ok(0), |ty| {
                    measure(ty, &words[v.offset..v.offset + ty.scalar_types().len()])
                })?
            }
            _ => 0,
        };
        children
            .checked_add(std::mem::size_of::<TaskValue>())
            .ok_or_else(|| invalid("Parallel result size overflow"))
    }
    fn read(ty: &Type, words: &[u64]) -> Result<TaskValue, Error> {
        if ty.is_place() {
            return unsafe { places::read(ty, words[0] as *const u8) };
        }
        Ok(match ty {
            Type::Record(r) if r.array => {
                let mut elements = Vec::with_capacity(unsafe { arrays::length(words[0])? });
                unsafe {
                    arrays::visit(ty, words[0], false, |element, words| {
                        elements.push(read(element, words)?);
                        Ok(())
                    })?;
                }
                TaskValue::Array {
                    ty: ty.clone(),
                    elements,
                }
            }
            Type::Record(r) if r.list => {
                let mut elements = Vec::with_capacity(unsafe { lists::length(ty, words)? });
                unsafe {
                    lists::visit(ty, words, false, |element, words| {
                        elements.push(read(element, words)?);
                        Ok(())
                    })?;
                }
                TaskValue::List {
                    ty: ty.clone(),
                    elements,
                }
            }
            Type::String => TaskValue::String(
                unsafe { crate::runtime::owned_text::borrow(words[0] as *const _) }?.to_owned(),
            ),
            Type::Tuple(t) => {
                let mut base = 0;
                let mut values = Vec::with_capacity(t.elements.len());
                for ty in &t.elements {
                    let end = base + ty.scalar_types().len();
                    values.push(read(ty, &words[base..end])?);
                    base = end;
                }
                TaskValue::Tuple(values)
            }
            Type::Record(r) => {
                let fields = r
                    .fields
                    .iter()
                    .map(|f| {
                        read(
                            &f.ty,
                            &words[f.offset..f.offset + f.ty.scalar_types().len()],
                        )
                    })
                    .collect::<Result<_, _>>()?;
                if r.map {
                    TaskValue::Map(TaskMap {
                        ty: ty.clone(),
                        fields,
                    })
                } else if r.set {
                    TaskValue::Set(TaskSet {
                        ty: ty.clone(),
                        fields,
                    })
                } else {
                    TaskValue::Record {
                        ty: ty.clone(),
                        fields,
                    }
                }
            }
            Type::Enum(e) => {
                let variant = words[0] as usize;
                let v = &e.variants[variant];
                TaskValue::Enum {
                    ty: ty.clone(),
                    variant,
                    payload: v
                        .payload
                        .as_ref()
                        .map(|ty| {
                            read(ty, &words[v.offset..v.offset + ty.scalar_types().len()])
                                .map(Box::new)
                        })
                        .transpose()?,
                }
            }
            _ => TaskValue::Scalar(
                InstanceValue::from_bits(ty, words.first().copied().unwrap_or(0))
                    .ok_or_else(|| invalid("Invalid parallel scalar result"))?,
            ),
        })
    }
    let charge = root.accounts().reserve(measure(ty, &packet.words)?)?;
    Ok(ParallelOutput {
        value: read(ty, &packet.words)?,
        _charge: charge,
    })
}
