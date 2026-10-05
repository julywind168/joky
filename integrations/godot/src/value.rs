//! Explicit, range-checked Godot Variant conversion at the script boundary.
use godot::{builtin::*, meta::error::CallErrorType, prelude::*};
use joky::{vm::InstanceValue as V, Type};

pub fn variant_type(ty: &Type) -> VariantType {
    match ty {
        Type::Unit => VariantType::NIL,
        Type::Bool => VariantType::BOOL,
        Type::Rune => VariantType::STRING,
        Type::Float32 | Type::Float64 => VariantType::FLOAT,
        _ => VariantType::INT,
    }
}
pub fn input(value: &Variant, ty: &Type) -> Result<V, CallErrorType> {
    let invalid = CallErrorType::InvalidArgument;
    if value.get_type() != variant_type(ty) {
        return Err(invalid);
    }
    macro_rules! integer {
        ($variant:ident, $rust:ty) => {
            V::$variant(
                <$rust>::try_from(value.try_to::<i64>().map_err(|_| invalid)?)
                    .map_err(|_| invalid)?,
            )
        };
    }
    Ok(match ty {
        Type::Unit => V::Unit,
        Type::Bool => V::Bool(value.try_to().map_err(|_| invalid)?),
        Type::Rune => {
            let text = value.try_to::<GString>().map_err(|_| invalid)?.to_string();
            let mut chars = text.chars();
            let ch = chars.next().ok_or(invalid)?;
            if chars.next().is_some() {
                return Err(invalid);
            }
            V::Rune(ch)
        }
        Type::Int8 => integer!(Int8, i8),
        Type::Int16 => integer!(Int16, i16),
        Type::Int32 => integer!(Int32, i32),
        Type::Int64 => integer!(Int64, i64),
        Type::UInt8 => integer!(UInt8, u8),
        Type::UInt16 => integer!(UInt16, u16),
        Type::UInt32 => integer!(UInt32, u32),
        Type::UInt64 => integer!(UInt64, u64),
        Type::Float32 => {
            let v = value.try_to::<f64>().map_err(|_| invalid)?;
            let n = v as f32;
            if v.is_finite() && !n.is_finite() {
                return Err(invalid);
            }
            V::Float32(n)
        }
        Type::Float64 => V::Float64(value.try_to().map_err(|_| invalid)?),
        _ => return Err(invalid),
    })
}
pub fn output(value: V) -> Result<Variant, CallErrorType> {
    Ok(match value {
        V::Unit => Variant::nil(),
        V::Bool(v) => v.to_variant(),
        V::Rune(v) => GString::from(&v.to_string()).to_variant(),
        V::Int8(v) => i64::from(v).to_variant(),
        V::Int16(v) => i64::from(v).to_variant(),
        V::Int32(v) => i64::from(v).to_variant(),
        V::Int64(v) => v.to_variant(),
        V::UInt8(v) => i64::from(v).to_variant(),
        V::UInt16(v) => i64::from(v).to_variant(),
        V::UInt32(v) => i64::from(v).to_variant(),
        V::UInt64(v) => i64::try_from(v)
            .map_err(|_| CallErrorType::InvalidArgument)?
            .to_variant(),
        V::Float32(v) => f64::from(v).to_variant(),
        V::Float64(v) => v.to_variant(),
    })
}
