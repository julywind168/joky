//! Internal domain module ABI v1. All addresses are relocations owned by the caller.
use crate::{
    core::{
        ir::FunctionInfo,
        parser::ast::{fresh_type_id, RecordField, RecordType},
    },
    Error, ErrorCode, ErrorPhase, FunctionSignature, Span, Type,
};
use std::{collections::HashMap, sync::Arc};

#[repr(C)]
pub(crate) struct ModuleV1 {
    pub version: usize,
    pub count: usize,
    pub entries: *const EntryV1,
}
#[repr(C)]
pub(crate) struct EntryV1 {
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
        "Invalid domain module descriptor v1",
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
        Type::Reference(r) => {
            out.push(if r.mutable { 15 } else { 14 });
            encode_type(out, &r.target);
        }
        Type::Record(r) => {
            out.push(13);
            put_u64(out, r.id);
            put_text(out, &r.name);
            out.push(u8::from(r.repr_c));
            put_u64(out, r.fields.len() as u64);
            for field in &r.fields {
                put_text(out, &field.name);
                encode_type(out, &field.ty);
            }
        }
        _ => unreachable!("checked scalar domain metadata"),
    }
}
pub(crate) fn encode(info: &FunctionInfo) -> Vec<u8> {
    let mut out = Vec::new();
    put_u64(&mut out, info.signature.params.len() as u64);
    for ty in &info.signature.params {
        encode_type(&mut out, ty);
    }
    encode_type(&mut out, &info.signature.ret);
    out
}
struct Reader<'a> {
    bytes: &'a [u8],
    records: &'a mut HashMap<u64, Type>,
}
impl Reader<'_> {
    fn byte(&mut self) -> Result<u8, Error> {
        let (&byte, rest) = self.bytes.split_first().ok_or_else(invalid)?;
        self.bytes = rest;
        Ok(byte)
    }
    fn number(&mut self) -> Result<usize, Error> {
        let b = self.bytes.get(..8).ok_or_else(invalid)?;
        let n = u64::from_le_bytes(b.try_into().unwrap());
        self.bytes = &self.bytes[8..];
        usize::try_from(n).map_err(|_| invalid())
    }
    fn text(&mut self) -> Result<String, Error> {
        let n = self.number()?;
        let b = self.bytes.get(..n).ok_or_else(invalid)?;
        let s = std::str::from_utf8(b).map_err(|_| invalid())?.to_owned();
        self.bytes = &self.bytes[n..];
        Ok(s)
    }
    fn ty(&mut self) -> Result<Type, Error> {
        let code = self.byte()?;
        if let Some(ty) = scalar(code) {
            return Ok(ty);
        }
        if code == 14 || code == 15 {
            let ty = self.ty()?;
            return Ok(Type::reference(ty, code == 15));
        }
        if code != 13 {
            return Err(invalid());
        }
        let id = self.number()? as u64;
        let name = self.text()?;
        let repr_c = self.byte()? != 0;
        let n = self.number()?;
        if n > 4096 {
            return Err(invalid());
        }
        let mut fields = Vec::new();
        let mut scalars = Vec::new();
        for _ in 0..n {
            let name = self.text()?;
            let ty = self.ty()?;
            if !crate::core::instance::scalar(&ty) {
                return Err(invalid());
            }
            let offset = scalars.len();
            scalars.extend(ty.scalar_types().iter().cloned());
            fields.push(RecordField {
                private: true,
                name,
                ty,
                offset,
            });
        }
        if let Some(ty) = self.records.get(&id) {
            return Ok(ty.clone());
        }
        let ty = Type::Record(Arc::new(RecordType {
            closure: None,
            owners: Vec::new(),
            repr_c,
            indirect: false,
            id: fresh_type_id(),
            name,
            parameters: Vec::new(),
            arguments: Vec::new(),
            template: None,
            fields,
            scalars,
            depth: 1,
        }));
        self.records.insert(id, ty.clone());
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
    entry: &EntryV1,
    records: &mut HashMap<u64, Type>,
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
    if !r.bytes.is_empty() {
        return Err(invalid());
    }
    let mut info = FunctionInfo::indirect(
        FunctionSignature::new(params, ret),
        entry.kind == 1,
        Span::empty(0),
    );
    info.frame_slots = entry.slots;
    info.internal = false;
    info.instance_entry = entry.kind == 0;
    info.domain_entry = entry.kind == 0;
    info.noescape = entry.kind == 0;
    Ok((name, info))
}
