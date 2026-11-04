//! Internal domain module ABI v4. All addresses are relocations owned by the caller.
use crate::core::types::{
    fresh_type_id, EnumType, EnumVariant, RecordField, RecordType, TupleType,
};
use crate::{core::ir::FunctionInfo, Error, ErrorCode, ErrorPhase, FunctionSignature, Span, Type};
use std::{collections::HashMap, sync::Arc};

#[repr(C)]
pub(crate) struct ModuleV4 {
    pub version: usize,
    pub count: usize,
    pub entries: *const EntryV4,
}
#[repr(C)]
pub(crate) struct EntryV4 {
    pub name: *const u8,
    pub name_len: usize,
    pub signature: *const u8,
    pub signature_len: usize,
    pub entry: *const u8,
    pub slots: usize,
    pub kind: usize,
}
fn invalid() -> Error {
    Error::new_with_code(
        ErrorPhase::Runtime,
        ErrorCode::AbiError,
        "Invalid domain module descriptor v4",
    )
}
fn scalar(code: u8) -> Option<Type> {
    Some(match code {
        0 => Type::Unit,
        1 => Type::Bool,
        2 => Type::Rune,
        3 => Type::Int8,
        4 => Type::Int16,
        5 => Type::Int32,
        6 => Type::Int64,
        7 => Type::UInt8,
        8 => Type::UInt16,
        9 => Type::UInt32,
        10 => Type::UInt64,
        11 => Type::Float32,
        12 => Type::Float64,
        _ => return None,
    })
}
fn put_u64(out: &mut Vec<u8>, n: u64) {
    out.extend(n.to_le_bytes());
}
fn put_text(out: &mut Vec<u8>, text: &str) {
    put_u64(out, text.len() as u64);
    out.extend(text.as_bytes());
}
fn encode_type(out: &mut Vec<u8>, ty: &Type) {
    if let Some(code) = (0..=12).find(|code| scalar(*code).as_ref() == Some(ty)) {
        out.push(code);
        return;
    }
    match ty {
        Type::String => out.push(16),
        Type::Buffer(element) => {
            out.push(17);
            encode_type(out, element);
        }
        Type::Tuple(t) => {
            out.push(18);
            put_u64(out, t.elements.len() as u64);
            for element in &t.elements {
                encode_type(out, element);
            }
        }
        Type::Enum(e) => {
            out.push(19);
            put_u64(out, e.id);
            put_text(out, &e.name);
            out.push(0);
            put_u64(out, e.arguments.len() as u64);
            for argument in &e.arguments {
                encode_argument(out, argument);
            }
            put_u64(out, e.variants.len() as u64);
            for variant in &e.variants {
                put_text(out, &variant.name);
                out.push(u8::from(variant.payload.is_some()));
                if let Some(payload) = &variant.payload {
                    encode_type(out, payload);
                }
            }
        }
        Type::Reference(r) => {
            out.push(if r.mutable { 15 } else { 14 });
            encode_type(out, &r.target);
        }
        Type::Record(r) => {
            out.push(if r.array { 21 } else { 13 });
            put_u64(out, r.id);
            put_text(out, &r.name);
            out.push(u8::from(r.repr_c));
            put_u64(out, r.arguments.len() as u64);
            for argument in &r.arguments {
                encode_argument(out, argument);
            }
            put_u64(out, r.fields.len() as u64);
            for field in &r.fields {
                put_text(out, &field.name);
                out.push(u8::from(field.private));
                encode_type(out, &field.ty);
            }
        }
        _ => unreachable!("checked domain metadata"),
    }
}
pub(crate) fn encode(info: &FunctionInfo) -> Vec<u8> {
    let mut out = Vec::new();
    put_u64(&mut out, info.signature.params.len() as u64);
    for ty in &info.signature.params {
        encode_type(&mut out, ty);
    }
    encode_type(&mut out, &info.signature.ret);
    if let Some(period) = info.every {
        // Optional scheduling metadata; ordinary v4 signatures are unchanged.
        put_u64(&mut out, period);
    }
    out
}
struct Reader<'a> {
    bytes: &'a [u8],
    records: &'a mut HashMap<(u64, Vec<u8>), Type>,
}
impl Reader<'_> {
    fn byte(&mut self) -> Result<u8, Error> {
        let (&byte, rest) = self.bytes.split_first().ok_or_else(invalid)?;
        self.bytes = rest;
        Ok(byte)
    }
    fn unsigned(&mut self) -> Result<u64, Error> {
        let b = self.bytes.get(..8).ok_or_else(invalid)?;
        let n = u64::from_le_bytes(b.try_into().unwrap());
        self.bytes = &self.bytes[8..];
        Ok(n)
    }
    fn number(&mut self) -> Result<usize, Error> {
        usize::try_from(self.unsigned()?).map_err(|_| invalid())
    }
    fn text(&mut self) -> Result<String, Error> {
        let n = self.number()?;
        let b = self.bytes.get(..n).ok_or_else(invalid)?;
        let s = std::str::from_utf8(b).map_err(|_| invalid())?.to_owned();
        self.bytes = &self.bytes[n..];
        Ok(s)
    }
    fn ty(&mut self) -> Result<Type, Error> {
        self.ty_at(0)
    }
    fn ty_at(&mut self, depth: usize) -> Result<Type, Error> {
        if depth > 64 {
            return Err(invalid());
        }
        let code = self.byte()?;
        if let Some(ty) = scalar(code) {
            return Ok(ty);
        }
        if code == 16 {
            return Ok(Type::String);
        }
        if code == 17 {
            let element = self.ty_at(depth + 1)?;
            return Ok(Type::Buffer(Arc::new(element)));
        }
        if code == 14 || code == 15 {
            let ty = self.ty_at(depth + 1)?;
            return Ok(Type::reference(ty, code == 15));
        }
        if code == 18 {
            let count = self.number()?;
            if count > 4096 {
                return Err(invalid());
            }
            let elements = (0..count)
                .map(|_| self.ty_at(depth + 1))
                .collect::<Result<Vec<_>, _>>()?;
            return Ok(Type::Tuple(Arc::new(TupleType {
                owners: elements
                    .iter()
                    .flat_map(|ty| ty.buffer_elements().iter().cloned())
                    .collect(),
                scalars: elements
                    .iter()
                    .flat_map(|ty| ty.scalar_types().iter().cloned())
                    .collect(),
                depth: elements.iter().map(Type::layout_depth).max().unwrap_or(0) + 1,
                elements,
            })));
        }
        if code != 13 && code != 19 && code != 21 {
            return Err(invalid());
        }
        let id = self.number()? as u64;
        let name = self.text()?;
        let repr_c = self.byte()? != 0;
        let arity = self.number()?;
        if arity > 4096 {
            return Err(invalid());
        }
        let start = self.bytes;
        let mut arguments = Vec::new();
        for _ in 0..arity {
            arguments.push(if self.bytes.first() == Some(&20) {
                self.byte()?;
                let bytes = self.bytes.get(..8).ok_or_else(invalid)?;
                let n = u64::from_le_bytes(bytes.try_into().unwrap());
                self.bytes = &self.bytes[8..];
                crate::core::types::GenericArgument::Const(
                    crate::core::types::ConstArgument::Value(n),
                )
            } else {
                self.ty_at(depth + 1)?.into()
            });
        }
        let key = (id, start[..start.len() - self.bytes.len()].to_vec());
        let n = self.number()?;
        if n > 4096 {
            return Err(invalid());
        }
        if code == 19 {
            let mut variants = Vec::new();
            let mut scalars = vec![Type::Int32];
            let mut layout_depth = 1;
            for _ in 0..n {
                let name = self.text()?;
                let payload = match self.byte()? {
                    0 => None,
                    1 => Some(self.ty_at(depth + 1)?),
                    _ => return Err(invalid()),
                };
                let offset = scalars.len();
                if let Some(ty) = &payload {
                    layout_depth = layout_depth.max(ty.layout_depth() + 1);
                    scalars.extend(ty.scalar_types().iter().cloned());
                }
                variants.push(EnumVariant {
                    name,
                    payload,
                    offset,
                });
            }
            if let Some(ty) = self.records.get(&key) {
                return Ok(ty.clone());
            }
            let ty = Type::Enum(Arc::new(EnumType {
                compact: false,
                owners: variants
                    .iter()
                    .filter_map(|v| v.payload.as_ref())
                    .flat_map(|ty| ty.buffer_elements().iter().cloned())
                    .collect(),
                indirect: false,
                id: fresh_type_id(),
                parameters: Vec::new(),
                arguments,
                template: None,
                name,
                variants,
                scalars,
                depth: layout_depth,
            }));
            self.records.insert(key, ty.clone());
            return Ok(ty);
        }
        let mut fields = Vec::new();
        let mut scalars = Vec::new();
        for _ in 0..n {
            let name = self.text()?;
            let private = match self.byte()? {
                0 => false,
                1 => true,
                _ => return Err(invalid()),
            };
            let ty = self.ty_at(depth + 1)?;
            if !crate::core::instance::scalar(&ty)
                && ty != Type::String
                && !matches!(
                    &ty,
                    Type::Buffer(_) | Type::Record(_) | Type::Tuple(_) | Type::Enum(_)
                )
            {
                return Err(invalid());
            }
            let offset = scalars.len();
            scalars.extend(ty.scalar_types().iter().cloned());
            fields.push(RecordField {
                private,
                name,
                ty,
                offset,
            });
        }
        if let Some(ty) = self.records.get(&key) {
            return Ok(ty.clone());
        }
        let depth = fields
            .iter()
            .map(|field| field.ty.layout_depth())
            .max()
            .unwrap_or(0)
            + 1;
        let ty = Type::Record(Arc::new(RecordType {
            custom_drop: false,
            array: code == 21,
            list: false,
            map: false,
            set: false,
            closure: None,
            owners: fields
                .iter()
                .flat_map(|field| field.ty.buffer_elements().iter().cloned())
                .collect(),
            repr_c,
            indirect: false,
            id: fresh_type_id(),
            name,
            parameters: Vec::new(),
            arguments,
            template: None,
            fields,
            scalars,
            depth,
        }));
        self.records.insert(key, ty.clone());
        Ok(ty)
    }
}
/// Decode only compiler-emitted immutable descriptors whose code owner is live.
///
/// # Safety
/// All name/signature slices and entry pointers must refer to the compiler-emitted
/// table, with its declared lengths and native layout, on the execution thread.
/// Its code and immutable data must remain live until every derived root stops;
/// raw C callers retain them explicitly rather than supplying a Rust code owner.
pub(crate) unsafe fn decode(
    entry: &EntryV4,
    records: &mut HashMap<(u64, Vec<u8>), Type>,
) -> Result<(String, FunctionInfo), Error> {
    if entry.name_len > 1048576
        || entry.signature_len > 1048576
        || entry.entry.is_null()
        || entry.kind > 1
    {
        return Err(invalid());
    }
    let name = std::str::from_utf8(std::slice::from_raw_parts(entry.name, entry.name_len))
        .map_err(|_| invalid())?
        .to_owned();
    let mut r = Reader {
        bytes: std::slice::from_raw_parts(entry.signature, entry.signature_len),
        records,
    };
    let n = r.number()?;
    if n > 1024 {
        return Err(invalid());
    }
    let mut params = Vec::new();
    for _ in 0..n {
        params.push(r.ty()?)
    }
    let ret = r.ty()?;
    let every = if r.bytes.is_empty() {
        None
    } else {
        let period = r.unsigned()?;
        if entry.kind != 0 || period == 0 {
            return Err(invalid());
        }
        Some(period)
    };
    if !r.bytes.is_empty() {
        return Err(invalid());
    }
    let mut info = FunctionInfo::indirect(
        FunctionSignature::new(params, ret),
        entry.kind == 1,
        Span::empty(0),
    );
    info.frame_slots = entry.slots;
    info.every = every;
    info.internal = false;
    info.instance_entry = entry.kind == 0;
    info.domain_entry = entry.kind == 0;
    info.domain_request_entry = entry.kind == 1;
    info.noescape = entry.kind == 0;
    Ok((name, info))
}

fn encode_argument(out: &mut Vec<u8>, a: &crate::core::types::GenericArgument) {
    match a {
        crate::core::types::GenericArgument::Type(t) => encode_type(out, t),
        crate::core::types::GenericArgument::Const(crate::core::types::ConstArgument::Value(n)) => {
            out.push(20);
            put_u64(out, *n);
        }
        _ => unreachable!("concrete domain argument"),
    }
}

/// Private owner descriptors use the same relocatable type encoding as domain entries.
pub(crate) fn encode_value_type(ty: &Type) -> Vec<u8> {
    let mut bytes = Vec::new();
    encode_type(&mut bytes, ty);
    bytes
}
pub(crate) fn decode_value_type(bytes: &[u8]) -> Result<Type, Error> {
    let mut records = HashMap::new();
    let mut reader = Reader {
        bytes,
        records: &mut records,
    };
    let ty = reader.ty_at(0)?;
    if !reader.bytes.is_empty() {
        return Err(invalid());
    }
    Ok(ty)
}

#[cfg(test)]
mod const_tests {
    use super::*;
    #[test]
    fn duration_period_metadata_preserves_full_uint64_and_rejects_zero() {
        for nanos in [1, 250_000, u64::MAX] {
            let mut info = FunctionInfo::indirect(
                FunctionSignature::new(vec![], Type::Unit),
                false,
                Span::empty(0),
            );
            info.every = Some(nanos);
            let signature = encode(&info);
            let entry = EntryV4 {
                name: b"tick".as_ptr(),
                name_len: 4,
                signature: signature.as_ptr(),
                signature_len: signature.len(),
                entry: std::ptr::NonNull::<u8>::dangling().as_ptr(),
                slots: 1,
                kind: 0,
            };
            // Decode reads the borrowed name/signature; it does not invoke the code pointer.
            let (_, decoded) = unsafe { decode(&entry, &mut HashMap::new()) }.unwrap();
            assert_eq!(decoded.every, Some(nanos));
            let mut invalid = signature.clone();
            let len = invalid.len();
            invalid[len - 8..].fill(0);
            let entry = EntryV4 {
                signature: invalid.as_ptr(),
                ..entry
            };
            assert!(unsafe { decode(&entry, &mut HashMap::new()) }.is_err());
        }
    }
    #[test]
    fn fixed_array_f1_const_nominal_arguments_round_trip_without_host_width_loss() {
        let module=crate::TypeRegistry::new().with_prelude(false).resolve_aliases(&crate::parser::parse("struct Tag<const N: UInt64> { value: Int32 } fn a(x:Tag<18446744073709551615>) {} fn b(x:Tag<0>) {}").unwrap()).unwrap();
        let mut records = HashMap::new();
        let mut decoded = Vec::new();
        for item in module.items {
            let crate::parser::ast::Item::FnDef(f) = item else {
                continue;
            };
            let original = &f.params[0].ty;
            let mut bytes = Vec::new();
            encode_type(&mut bytes, original);
            let mut reader = Reader {
                bytes: &bytes,
                records: &mut records,
            };
            let copy = reader.ty().unwrap();
            assert!(reader.bytes.is_empty());
            let (Type::Record(a), Type::Record(b)) = (original, &copy) else {
                panic!()
            };
            assert_eq!(a.arguments, b.arguments);
            assert_eq!(
                crate::core::script::schema(original),
                crate::core::script::schema(&copy)
            );
            decoded.push(copy);
        }
        assert_ne!(decoded[0], decoded[1]);
    }
}
