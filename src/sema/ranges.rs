use std::collections::HashMap;

use super::checker::{intern_option, intern_tuple, Checker};
use super::expectation::TypeExpectation;
use super::symbol_table::{FunctionSignature, StructInfo};
use super::{type_name, Type};
use crate::diagnostic::SemanticError;
use crate::syntax::Expr;
use crate::Span;

const RANGE_PREFIX: &str = "@builtin/Range(";

impl Checker {
    pub(super) fn range_item(&self, ty: Type) -> Option<Type> {
        let Type::Struct(id) = ty else { return None };
        self.structs.iter().find_map(|(name, info)| {
            (info.id == id && name.starts_with(RANGE_PREFIX)).then(|| info.fields[0].1)
        })
    }

    pub(super) fn intern_range(&mut self, item: Type, span: Span) -> Result<Type, SemanticError> {
        if !item.is_integer() {
            return Err(SemanticError::FunctionNotSupported {
                name: "Range requires a concrete integer type".into(),
                span,
            });
        }
        // Canonical nominal layouts reuse value ownership, ABI relocation and
        // continuation spills without introducing another runtime object kind.
        let name = format!("{RANGE_PREFIX}{})", type_name(item));
        if let Some(info) = self.structs.get(&name) {
            return Ok(Type::Struct(info.id));
        }
        let id = self.structs.len();
        let ty = Type::Struct(id);
        let pair = Type::Tuple(intern_tuple(&mut self.tuple_types, vec![item, ty]));
        let result = Type::Option(intern_option(&mut self.option_types, pair));
        self.structs.insert(
            name,
            StructInfo {
                id,
                repr_c: false,
                fields: vec![
                    ("@current".into(), item),
                    ("@end".into(), item),
                    ("@step".into(), item),
                    ("@inclusive".into(), Type::Bool),
                    ("@exhausted".into(), Type::Bool),
                ],
                defaults: vec![None; 5],
                methods: HashMap::from([(
                    super::CURSOR_METHOD.into(),
                    FunctionSignature {
                        receiver_mode: crate::syntax::ReceiverMode::Owned,
                        parameter_borrows: vec![],
                        type_parameters: vec![],
                        type_parameter_bounds: vec![],
                        associated_bounds: vec![],
                        parameters: vec![],
                        return_type: result,
                        declared_effects: super::EffectGroupSet::new(),
                        used_effects: super::EffectSet::new(),
                    },
                )]),
            },
        );
        self.trait_impls
            .entry("Cursor".into())
            .or_default()
            .insert(ty);
        self.trait_associated_impls.insert(
            ("Cursor".into(), ty),
            HashMap::from([("Item".into(), item)]),
        );
        Ok(ty)
    }

    pub(super) fn check_range(
        &mut self,
        start: &Expr,
        end: &Expr,
        step: Option<&Expr>,
        expectation: TypeExpectation,
    ) -> Result<Type, SemanticError> {
        let operands: Vec<_> = [Some(start), Some(end), step]
            .into_iter()
            .flatten()
            .collect();
        let hinted = expectation.ty().and_then(|ty| self.range_item(ty));
        let anchor = operands
            .iter()
            .copied()
            .find(|expr| !super::validation::is_numeric_literal(expr))
            .unwrap_or(start);
        let item = match hinted {
            Some(ty) => ty,
            None => self.check_expression(anchor, TypeExpectation::none())?,
        };
        let range = self.intern_range(item, start.span.merge(end.span))?;
        for operand in operands {
            self.check_expression(operand, TypeExpectation::require(item))?;
        }
        if step.is_some_and(|expr| super::validation::constant_integer(expr) == Some(0)) {
            return Err(SemanticError::FunctionNotSupported {
                name: "range step must not be zero".into(),
                span: step.unwrap().span,
            });
        }
        Ok(range)
    }
}

impl super::TypeTable {
    pub(crate) fn range_item(&self, ty: Type) -> Option<Type> {
        let Type::Struct(id) = ty else { return None };
        self.struct_name(id)
            .starts_with(RANGE_PREFIX)
            .then(|| self.struct_fields(id)[0].1)
    }
}
