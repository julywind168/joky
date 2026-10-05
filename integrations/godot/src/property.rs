//! Export conventions are static metadata; editor queries never execute Joky.
use crate::runtime::{Diagnostic, Method};
use joky::{
    parser::ast::{Expr, FnDef, Item, Literal, UnaryOp},
    vm::{InstanceValue as V, VmEngine},
    Type,
};
use std::collections::BTreeMap;

pub struct Property {
    pub ty: Type,
    pub default: V,
    pub getter: String,
    pub setter: String,
    pub line: i32,
}
fn literal(expr: &Expr) -> Option<(&Literal, bool)> {
    match expr {
        Expr::Literal { literal, .. } => Some((literal, false)),
        Expr::Block { exprs, .. } if exprs.len() == 1 => literal(&exprs[0]),
        Expr::Unary {
            op: UnaryOp::Neg,
            operand,
            ..
        } => literal(operand).map(|(v, sign)| (v, !sign)),
        _ => None,
    }
}
fn field_getter(expr: &Expr, receiver: &str) -> bool {
    match expr {
        Expr::Block { exprs, .. } if exprs.len() == 1 => field_getter(&exprs[0], receiver),
        Expr::Field { base, .. } => {
            matches!(base.as_ref(), Expr::Ident { name, .. } if name == receiver)
        }
        _ => false,
    }
}
fn default_value(function: &FnDef, ty: &Type) -> Option<V> {
    let (literal, negative) = literal(&function.body)?;
    let number = match literal {
        Literal::Float32(v) => v.to_string(),
        Literal::Float64(v) => v.to_string(),
        Literal::Float(v) => v.replace('_', ""),
        Literal::Integer {
            magnitude,
            negative,
        } => format!("{}{magnitude}", if *negative { "-" } else { "" }),
        Literal::Int8(v) => v.to_string(),
        Literal::Int16(v) => v.to_string(),
        Literal::Int32(v) => v.to_string(),
        Literal::Int64(v) => v.to_string(),
        Literal::UInt8(v) => v.to_string(),
        Literal::UInt16(v) => v.to_string(),
        Literal::UInt32(v) => v.to_string(),
        Literal::UInt64(v) => v.to_string(),
        Literal::Bool(v) if *ty == Type::Bool && !negative => return Some(V::Bool(*v)),
        Literal::Rune(v) if *ty == Type::Rune && !negative => {
            return char::from_u32(*v).map(V::Rune)
        }
        _ => return None,
    };
    let number = if negative {
        format!("-{number}")
    } else {
        number
    };
    macro_rules! parse { ($($ty:ident),*) => { match ty { $(Type::$ty => number.parse().ok().map(V::$ty),)* _ => None } }; }
    parse!(Int8, Int16, Int32, Int64, UInt8, UInt16, UInt32, UInt64, Float32, Float64)
}
pub fn collect(
    path: &str,
    source: &str,
    items: &[Item],
    vm: &VmEngine,
    methods: &BTreeMap<String, Method>,
) -> Result<BTreeMap<String, Property>, Diagnostic> {
    let mut properties = BTreeMap::new();
    for item in items {
        let Item::FnDef(function) = item else {
            continue;
        };
        let Some(name) = function.name.strip_prefix("default_") else {
            continue;
        };
        let invalid = || {
            Diagnostic::contract(path, source, function.span.start(), "Exported properties require a literal default_NAME() and checked get_NAME(&State)/set_NAME(&mut State, value) @noescape methods of the same scalar type")
        };
        if name.is_empty() || name.starts_with('_') || properties.len() >= 128 {
            return Err(invalid());
        }
        let getter = format!("get_{name}");
        let setter = format!("set_{name}");
        let get = methods.get(&getter).ok_or_else(invalid)?;
        let set = methods.get(&setter).ok_or_else(invalid)?;
        let ty = &get.signature.ret;
        let pure = items.iter().any(|item| matches!(item, Item::FnDef(f) if f.name == getter && f.params.first().is_some_and(|p| field_getter(&f.body, &p.name))));
        if !pure {
            return Err(Diagnostic::contract(path, source, function.span.start(), "Exported property getters must directly read a scalar State field without engine callbacks"));
        }
        if get.signature.params.len() != 1
            || !matches!(&get.signature.params[0], Type::Reference(r) if !r.mutable)
            || set.signature.params.len() != 2
            || !matches!(&set.signature.params[0], Type::Reference(r) if r.mutable)
            || set.signature.params[1] != *ty
            || set.signature.ret != Type::Unit
            || !vm
                .function_signature(&function.name)
                .is_some_and(|s| s.params.is_empty() && s.ret == *ty)
            || function.suspends
            || !function.type_params.is_empty()
        {
            return Err(invalid());
        }
        let default = default_value(function, ty).ok_or_else(invalid)?;
        // Godot int is signed; exported defaults must be representable.
        if matches!(default, V::UInt64(v) if v > i64::MAX as u64) {
            return Err(invalid());
        }
        properties.insert(
            name.into(),
            Property {
                ty: ty.clone(),
                default,
                getter,
                setter,
                line: get.line,
            },
        );
    }
    Ok(properties)
}
