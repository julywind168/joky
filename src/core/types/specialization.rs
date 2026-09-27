//! Bounded enum specialization and substitution shared with generic functions.
use super::*;
use crate::parser::ast::{EnumType, EnumVariant, TupleType};
use std::sync::Arc;

impl TypeRegistry {
    pub(crate) fn instantiate_enum(
        &self,
        enumeration: &Arc<EnumType>,
        arguments: Vec<Type>,
    ) -> Result<Type, Error> {
        let template = enumeration.template.as_ref().unwrap_or(enumeration);
        if template.parameters.is_empty() || template.parameters.len() != arguments.len() {
            return Err(error(
                ErrorCode::TypeMismatch,
                format!(
                    "Type argument count mismatch for enum `{}`: expected {}",
                    template.name,
                    template.parameters.len()
                ),
            ));
        }
        for arg in &arguments {
            self.validate(arg)?;
            if !arg.is_copy_scalar() && !arg.is_aggregate() {
                return Err(error(
                    ErrorCode::TypeMismatch,
                    format!("Generic enum argument must be a Copy type, got {arg}"),
                ));
            }
        }
        if let Some(instance) = self
            .enum_instances
            .borrow()
            .iter()
            .find(|e| e.id == template.id && e.arguments == arguments)
        {
            return Ok(Type::Enum(instance.clone()));
        }
        if self.enum_instances.borrow().len() >= 256 {
            return Err(error(
                ErrorCode::ResourceLimit,
                "Generic enum instantiation exceeds 256 instances",
            ));
        }
        let substitutions = template
            .parameters
            .iter()
            .zip(&arguments)
            .map(|(p, ty)| (p.id, ty.clone()))
            .collect();
        let mut scalars = vec![Type::Int32];
        let mut depth = arguments
            .iter()
            .map(|ty| ty.layout_depth() + 1)
            .max()
            .unwrap_or(1);
        if depth > 256 {
            return Err(error(
                ErrorCode::ResourceLimit,
                "Enum type nesting exceeds 256 levels",
            ));
        }
        let mut variants = Vec::new();
        for variant in &template.variants {
            let offset = scalars.len();
            let payload = variant
                .payload
                .as_ref()
                .map(|ty| substitute(ty, &substitutions, self))
                .transpose()?;
            if let Some(ty) = &payload {
                layout::extend_layout(&mut scalars, &mut depth, ty, crate::Span::empty(0), "Enum")?;
            }
            variants.push(EnumVariant {
                name: variant.name.clone(),
                payload,
                offset,
            });
        }
        let instance = Arc::new(EnumType {
            id: template.id,
            name: template.name.clone(),
            parameters: template.parameters.clone(),
            arguments,
            template: Some(template.clone()),
            variants,
            scalars,
            depth,
        });
        // Nested payload applications may also have populated the shared cache.
        if self.enum_instances.borrow().len() >= 256 {
            return Err(error(
                ErrorCode::ResourceLimit,
                "Generic enum instantiation exceeds 256 instances",
            ));
        }
        self.enum_instances.borrow_mut().push(instance.clone());
        Ok(Type::Enum(instance))
    }
}

pub(crate) fn substitute(
    ty: &Type,
    substitutions: &HashMap<u64, Type>,
    types: &TypeRegistry,
) -> Result<Type, Error> {
    #[derive(Default)]
    struct Cache {
        tuples: HashMap<*const TupleType, Type>,
        enums: HashMap<*const EnumType, Type>,
    }
    fn visit(
        ty: &Type,
        substitutions: &HashMap<u64, Type>,
        types: &TypeRegistry,
        cache: &mut Cache,
    ) -> Result<Type, Error> {
        Ok(match ty {
            Type::Parameter(p) => substitutions
                .get(&p.id)
                .cloned()
                .unwrap_or_else(|| ty.clone()),
            Type::Tuple(tuple) => {
                let key = Arc::as_ptr(tuple);
                if let Some(ty) = cache.tuples.get(&key) {
                    return Ok(ty.clone());
                }
                let elements = tuple
                    .elements
                    .iter()
                    .map(|ty| visit(ty, substitutions, types, cache))
                    .collect::<Result<_, _>>()?;
                let ty = tuple_type(elements, types)?;
                cache.tuples.insert(key, ty.clone());
                ty
            }
            Type::Enum(enumeration) if !enumeration.parameters.is_empty() => {
                let key = Arc::as_ptr(enumeration);
                if let Some(ty) = cache.enums.get(&key) {
                    return Ok(ty.clone());
                }
                let arguments = enumeration
                    .arguments
                    .iter()
                    .map(|ty| visit(ty, substitutions, types, cache))
                    .collect::<Result<_, _>>()?;
                let ty = types.instantiate_enum(enumeration, arguments)?;
                cache.enums.insert(key, ty.clone());
                ty
            }
            _ => ty.clone(),
        })
    }
    visit(ty, substitutions, types, &mut Cache::default())
}

fn error(code: ErrorCode, message: impl Into<String>) -> Error {
    Error::new_with_code(ErrorPhase::TypeCheck, code, message)
}
