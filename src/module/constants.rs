//! Typed constant expressions contain no references into a defining module's AST.
use crate::sema::{CheckedTypes, Type, TypeTable};
use crate::syntax::{BinaryOp, Expr, ExprKind, UnaryOp};
use crate::Diagnostic;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct ConstantValue {
    pub ty: Type,
    pub kind: ConstantKind,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) enum ConstantKind {
    Integer(u64),
    Float(f64),
    Duration(u64),
    String(String),
    Bytes(Vec<u8>),
    Boolean(bool),
    Unary(UnaryOp, Box<ConstantValue>),
    Binary(BinaryOp, Box<ConstantValue>, Box<ConstantValue>),
    Tuple(Vec<ConstantValue>),
    Struct(Vec<(String, ConstantValue)>),
}

impl ConstantValue {
    pub(crate) fn canonical_bytes(
        &self,
        types: &TypeTable,
        module: super::StableId,
    ) -> Result<Vec<u8>, Diagnostic> {
        let mut value = self.clone();
        let mut type_keys = Vec::new();
        value
            .map_types(&mut |ty| {
                type_keys.push(types.stable_type_key(ty, module));
                Ok::<_, std::convert::Infallible>(Type::Unit)
            })
            .expect("infallible type normalization");
        bincode::serialize(&(type_keys, value))
            .map_err(|error| Diagnostic::codegen(error.to_string()))
    }

    pub(crate) fn from_expression(expr: &Expr, types: &CheckedTypes) -> Result<Self, Diagnostic> {
        if let Some(value) = types.imported_constants.get(&expr.id) {
            return Ok(value.clone());
        }
        if let Some(value) = types
            .constant_reference(expr.id)
            .and_then(|n| types.constant_value(n))
        {
            return Self::from_expression(value, types);
        }
        let kind = match &expr.kind {
            ExprKind::Integer(v) | ExprKind::TypedInteger(v, _) => ConstantKind::Integer(*v),
            ExprKind::Float(v) => ConstantKind::Float(*v),
            ExprKind::Duration(v) => ConstantKind::Duration(*v),
            ExprKind::String(v) => ConstantKind::String(v.clone()),
            ExprKind::IncludeBytes {
                data: Some(data), ..
            } => ConstantKind::Bytes(data.clone()),
            ExprKind::Boolean(v) => ConstantKind::Boolean(*v),
            ExprKind::Unary { op, expression } => {
                ConstantKind::Unary(*op, Box::new(Self::from_expression(expression, types)?))
            }
            ExprKind::Binary { op, left, right } => ConstantKind::Binary(
                *op,
                Box::new(Self::from_expression(left, types)?),
                Box::new(Self::from_expression(right, types)?),
            ),
            ExprKind::Tuple(elements) => ConstantKind::Tuple(
                elements
                    .iter()
                    .map(|v| Self::from_expression(v, types))
                    .collect::<Result<_, _>>()?,
            ),
            ExprKind::Call { arguments, .. }
                if matches!(types.get_optional(expr), Some(Type::Struct(_))) =>
            {
                let Type::Struct(id) = types.get(expr) else {
                    unreachable!()
                };
                let mut fields = Vec::new();
                for (index, (name, _)) in types.struct_fields(id).iter().enumerate() {
                    let value = arguments
                        .iter()
                        .find(|arg| arg.label.as_deref() == Some(name))
                        .map(|arg| &arg.value)
                        .or_else(|| types.struct_defaults().get(id)?.get(index)?.as_ref())
                        .ok_or_else(|| {
                            Diagnostic::codegen("missing imported struct default field")
                        })?;
                    fields.push((name.clone(), Self::from_expression(value, types)?));
                }
                ConstantKind::Struct(fields)
            }
            _ => {
                return Err(Diagnostic::codegen(
                    "unsupported exported constant expression",
                ))
            }
        };
        Ok(Self {
            ty: types.get(expr),
            kind,
        })
    }

    pub(crate) fn map_types<E>(
        &mut self,
        map: &mut impl FnMut(Type) -> Result<Type, E>,
    ) -> Result<(), E> {
        self.ty = map(self.ty)?;
        match &mut self.kind {
            ConstantKind::Unary(_, value) => value.map_types(map)?,
            ConstantKind::Binary(_, left, right) => {
                left.map_types(map)?;
                right.map_types(map)?;
            }
            ConstantKind::Tuple(values) => {
                for value in values {
                    value.map_types(map)?;
                }
            }
            ConstantKind::Struct(fields) => {
                for (_, value) in fields {
                    value.map_types(map)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}
