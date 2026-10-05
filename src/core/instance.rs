//! Compiler-owned eligibility rules for persistent reference-free roots.
use super::ir::FunctionInfo;
use crate::Type;

/// Typed scalar arguments and results of the persistent-instance API.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum InstanceValue {
    Unit,
    Bool(bool),
    Rune(char),
    Int8(i8),
    Int16(i16),
    Int32(i32),
    Int64(i64),
    UInt8(u8),
    UInt16(u16),
    UInt32(u32),
    UInt64(u64),
    Float32(f32),
    Float64(f64),
}

macro_rules! scalar_values {
    ($($name:ident($rust:ty)),* $(,)?) => {
        impl InstanceValue {
            pub fn value_type(self) -> Type {
                match self {
                    Self::Unit => Type::Unit,
                    Self::Bool(_) => Type::Bool,
                    Self::Rune(_) => Type::Rune,
                    Self::Float32(_) => Type::Float32,
                    Self::Float64(_) => Type::Float64,
                    $(Self::$name(_) => Type::$name,)*
                }
            }
            pub(crate) fn bits(self) -> u64 {
                match self {
                    Self::Unit => 0,
                    Self::Bool(v) => u64::from(v),
                    Self::Rune(v) => u64::from(u32::from(v)),
                    Self::Float32(v) => u64::from(v.to_bits()),
                    Self::Float64(v) => v.to_bits(),
                    $(Self::$name(v) => v as u64,)*
                }
            }
            pub(crate) fn from_bits(ty: &Type, bits: u64) -> Option<Self> {
                Some(match ty {
                    Type::Unit => Self::Unit,
                    Type::Bool if bits <= 1 => Self::Bool(bits != 0),
                    Type::Rune => Self::Rune(char::from_u32(u32::try_from(bits).ok()?)?),
                    Type::Float32 => Self::Float32(f32::from_bits(bits as u32)),
                    Type::Float64 => Self::Float64(f64::from_bits(bits)),
                    $(Type::$name => Self::$name(bits as $rust),)*
                    _ => return None,
                })
            }
        }
    }
}
scalar_values! {
    Int8(i8), Int16(i16), Int32(i32), Int64(i64),
    UInt8(u8), UInt16(u16), UInt32(u32), UInt64(u64),
}

pub(crate) fn scalar(ty: &Type) -> bool {
    ty.integer().is_some()
        || matches!(
            ty,
            Type::Float32 | Type::Float64 | Type::Bool | Type::Rune | Type::Unit
        )
}

/// Compiler-owned traversal rejects references, views, pointers and callable
/// values anywhere in the complete storage and specialization graph.
pub(crate) fn persistent(ty: &Type) -> bool {
    // Fields and Storage are complementary: Fields includes nominal arguments
    // and zero-sized fields; Storage reaches nested buffer element types.
    [
        super::types::walk::TypeEdges::Fields,
        super::types::walk::TypeEdges::Storage,
    ]
    .into_iter()
    .all(|edges| {
        super::types::walk::TypeWalk::new(ty, edges).all(|part| {
            scalar(part)
                || matches!(
                    part,
                    Type::String
                        | Type::Buffer(_)
                        | Type::Tuple(_)
                        | Type::Enum(_)
                        | Type::Record(_)
                )
        })
    }) && ty.storage_layout().is_ok()
}

pub(crate) fn constructor(info: &FunctionInfo) -> bool {
    info.instance_entry
        && !info.suspends
        && matches!(info.signature.ret, Type::Record(_))
        && persistent(&info.signature.ret)
        && info.signature.params.iter().all(scalar)
}

pub(crate) fn receiver(info: &FunctionInfo) -> Option<&Type> {
    if !info.instance_entry
        || info.suspends
        || !(info.noescape || info.noescape_params.contains(&0))
        || !scalar(&info.signature.ret)
    {
        return None;
    }
    let (first, rest) = info.signature.params.split_first()?;
    let Type::Reference(reference) = first else {
        return None;
    };
    (matches!(reference.target, Type::Record(_))
        && persistent(&reference.target)
        && rest.iter().all(scalar))
    .then_some(&reference.target)
}

pub(crate) fn eligible(info: &FunctionInfo) -> bool {
    constructor(info) || receiver(info).is_some()
}

pub(crate) fn symbol(prefix: &str, name: &str) -> String {
    format!("__joky_instance_entry_{prefix}{name}")
}
