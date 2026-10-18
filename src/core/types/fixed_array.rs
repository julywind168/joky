//! Compact fixed-array normalization and bounded inline work accounting.
use super::*;
use crate::parser::ast::{ConstArgument, FixedArrayType, ParameterKind};
use std::{collections::HashSet, sync::Arc};

pub(crate) const MAX_LENGTH: u64 = 65_536;
pub(crate) const MAX_BYTES: usize = 64 * 1024 * 1024;

fn failure(code: ErrorCode, message: &str) -> Error {
    Error::new_with_code(ErrorPhase::TypeCheck, code, message)
}

impl Type {
    /// Plain inline data accepted by fixed-array places and dynamic elements.
    /// Rigid template obligations are rechecked after specialization.
    pub(crate) fn is_fixed_array_data(&self) -> bool {
        TypeWalk::new(self, TypeEdges::Fields).all(|ty| match ty {
            Type::Record(r) => r.owners.is_empty() && r.closure.is_none(),
            Type::Enum(e) => e.owners.is_empty(),
            Type::Tuple(_)
            | Type::FixedArray(_)
            | Type::Unit
            | Type::Bool
            | Type::Rune
            | Type::Parameter(_)
            | Type::Projection(_) => true,
            _ => ty.integer().is_some() || ty.is_float(),
        })
    }
}

impl TypeRegistry {
    pub(crate) fn fixed_array(&self, element: Type, length: ConstArgument) -> Result<Type, Error> {
        let shape = Arc::new(FixedArrayType { element, length });
        let ty = Type::FixedArray(shape.clone());
        self.validate(&ty)?;
        if let Some(existing) = self.fixed_arrays.borrow().iter().find(|a| ***a == *shape) {
            return Ok(Type::FixedArray(existing.clone()));
        }
        if self.fixed_arrays.borrow().len() >= 256 {
            return Err(failure(
                ErrorCode::ResourceLimit,
                "FixedArray normalization exceeds 256 shapes",
            ));
        }
        self.fixed_arrays.borrow_mut().push(shape);
        Ok(ty)
    }
}

pub(super) fn validate(ty: &Type, registry: &TypeRegistry) -> Result<(), Error> {
    if !ty.contains_fixed_array() {
        return Ok(());
    }
    for node in TypeWalk::new(ty, TypeEdges::Fields) {
        let (Type::FixedArray(array) | Type::FixedArrayInit(array)) = node else {
            continue;
        };
        match &array.length {
            ConstArgument::Value(n) if *n <= MAX_LENGTH => {}
            ConstArgument::Parameter(p)
                if p.kind == ParameterKind::Const && registry.parameters.contains_key(&p.id) => {}
            ConstArgument::Value(n) => {
                return Err(failure(
                    ErrorCode::ResourceLimit,
                    &format!("FixedArray length {n} exceeds 65536"),
                ))
            }
            _ => {
                return Err(failure(
                    ErrorCode::TypeMismatch,
                    "FixedArray length parameter is outside its declaration",
                ))
            }
        }
        for element in TypeWalk::new(&array.element, TypeEdges::Fields) {
            let allowed = match element {
                Type::Float32
                | Type::Float64
                | Type::Int8
                | Type::Int16
                | Type::Int32
                | Type::Int64
                | Type::UInt8
                | Type::UInt16
                | Type::UInt32
                | Type::UInt64
                | Type::Bool
                | Type::Rune
                | Type::Unit
                | Type::FixedArray(_)
                | Type::Tuple(_) => true,
                Type::Record(r) => r.closure.is_none() && r.owners.is_empty() && !r.indirect,
                Type::Enum(e) => e.owners.is_empty() && !e.indirect,
                Type::Parameter(p) => {
                    p.kind == ParameterKind::Type
                        && p.callable.is_none()
                        && registry.parameters.contains_key(&p.id)
                }
                Type::Projection(_) => true,
                _ => false,
            };
            if !allowed {
                return Err(failure(
                    ErrorCode::TypeMismatch,
                    &format!(
                        "FixedArray element {element} requires finite reference-free Copy data"
                    ),
                ));
            }
        }
    }
    weight(ty)?;
    // Rigid declarations retain obligations; every concrete substitution checks them again.
    let rigid = TypeWalk::new(ty, TypeEdges::Fields).any(|t| {
        matches!(t, Type::Parameter(_) | Type::Projection(_))
            || matches!(t,Type::FixedArray(a) | Type::FixedArrayInit(a) if matches!(a.length,ConstArgument::Parameter(_)))
    });
    if !rigid {
        let target = ty.reference_type().map_or(ty, |r| &r.target);
        let shallow = matches!(target, Type::Record(r) if r.indirect)
            || matches!(target, Type::Enum(e) if e.indirect);
        // References may retain only nominal identity. Validate their concrete
        // array arguments now; the full object is rechecked on materialization.
        for target in TypeWalk::new(target, TypeEdges::Fields).filter(|node| {
            matches!(node, Type::FixedArray(_) | Type::FixedArrayInit(_))
                || !shallow && std::ptr::eq(*node, target)
        }) {
            let layout = if target.is_owner() {
                target.storage_layout()
            } else {
                target.memory_layout()
            };
            layout.map_err(|e| failure(ErrorCode::ResourceLimit,
                &format!("FixedArray memory layout for {ty}: {e}; maximum object size is {MAX_BYTES} bytes")))?;
        }
    }

    Ok(())
}

/// Count logical work, including zero-length dimensions and zero-sized leaves.
/// Memoized node weights are added for every edge, preserving multiplicity.
pub(super) fn weight(ty: &Type) -> Result<u64, Error> {
    fn visit(
        ty: &Type,
        depth: usize,
        cache: &mut HashMap<usize, u64>,
        active: &mut HashSet<usize>,
    ) -> Result<u64, Error> {
        if depth > 256 {
            return Err(failure(
                ErrorCode::ResourceLimit,
                "FixedArray nesting exceeds 256 levels",
            ));
        }
        let key = match ty {
            Type::FixedArray(a) | Type::FixedArrayInit(a) => Arc::as_ptr(a) as usize,
            Type::Record(r) => Arc::as_ptr(r) as usize,
            Type::Enum(e) => Arc::as_ptr(e) as usize,
            Type::Tuple(t) => Arc::as_ptr(t) as usize,
            _ => return Ok(1),
        };
        if let Some(n) = cache.get(&key) {
            return Ok(*n);
        }
        if !active.insert(key) {
            return Err(failure(
                ErrorCode::TypeMismatch,
                "Recursive inline FixedArray layout",
            ));
        }
        let mut child = |t| visit(t, depth + 1, cache, active);
        let n = match ty {
            Type::FixedArray(a) | Type::FixedArrayInit(a) => {
                let n = match a.length {
                    ConstArgument::Value(n) => n,
                    ConstArgument::Parameter(_) => 1,
                };
                if n > MAX_LENGTH {
                    return Err(failure(
                        ErrorCode::ResourceLimit,
                        &format!("FixedArray length {n} exceeds 65536"),
                    ));
                }
                n.max(1).saturating_mul(child(&a.element)?)
            }
            Type::Record(r) => r
                .fields
                .iter()
                .try_fold(0u64, |sum, f| {
                    Ok::<_, Error>(sum.saturating_add(child(&f.ty)?))
                })?
                .max(1),
            Type::Tuple(t) => t
                .elements
                .iter()
                .try_fold(0u64, |sum, t| Ok::<_, Error>(sum.saturating_add(child(t)?)))?
                .max(1),
            Type::Enum(e) => e
                .variants
                .iter()
                .filter_map(|v| v.payload.as_ref())
                .try_fold(1u64, |sum, t| Ok::<_, Error>(sum.saturating_add(child(t)?)))?,
            _ => unreachable!(),
        };
        if n > MAX_LENGTH {
            return Err(failure(
                ErrorCode::ResourceLimit,
                &format!("FixedArray logical expansion {n} exceeds 65536 for {ty}"),
            ));
        }
        active.remove(&key);
        cache.insert(key, n);
        Ok(n)
    }
    let mut cache = HashMap::new();
    let mut active = HashSet::new();
    let result = visit(ty, 0, &mut cache, &mut active)?;
    for node in TypeWalk::new(ty, TypeEdges::Fields) {
        visit(node, 0, &mut cache, &mut active)?;
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::types::memory_layout::{MemoryLayoutShape, MemoryLayoutTarget};
    use crate::parser::ast::{GenericArgument, Item};

    fn normalized(source: &str) -> Result<Module, Error> {
        TypeRegistry::new().with_prelude(false).resolve_aliases(
            &crate::parser::parse(&format!(
                "import joky/fixed_array.{{type FixedArray}}\n{source}"
            ))
            .map_err(Error::from)?,
        )
    }
    fn result(source: &str) -> Type {
        normalized(source)
            .unwrap()
            .items
            .iter()
            .find_map(|i| match i {
                Item::FnDef(f) if f.name == "f" => Some(f.ret_ty.clone()),
                _ => None,
            })
            .unwrap()
    }
    #[test]
    fn fixed_array_f1_identity_aliases_and_compact_cross_target_layouts() {
        let module = normalized("type A<const N: UInt64> = FixedArray<Int32,N> fn f(x: A<0x4>) -> A<0b100> { x } fn g(x: A<3>) -> A<3> { x }").unwrap();
        let functions: Vec<_> = module
            .items
            .iter()
            .filter_map(|i| match i {
                Item::FnDef(f) if f.name == "f" || f.name == "g" => Some(f),
                _ => None,
            })
            .collect();
        assert_eq!(functions[0].params[0].ty, functions[0].ret_ty);
        assert_ne!(functions[0].ret_ty, functions[1].ret_ty);
        for (pointer_size, int64_align) in [(4, 4), (8, 8)] {
            let target = MemoryLayoutTarget {
                pointer_size,
                pointer_align: pointer_size,
                int32_align: 4,
                int64_align,
                max_size: isize::MAX as usize,
            };
            for (source,size,align,stride) in [
                ("fn f(x: FixedArray<Int32,4>) -> FixedArray<Int32,4> { x }",16,4,4),
                ("fn f(x: FixedArray<FixedArray<Int32,3>,2>) -> FixedArray<FixedArray<Int32,3>,2> { x }",24,4,12),
                ("struct P { x: Int32, y: Int64 } fn f(x: FixedArray<P,3>) -> FixedArray<P,3> { x }",if pointer_size==4 {36} else {48},int64_align,if pointer_size==4 {12} else {16}),
                ("fn f(x: FixedArray<Float64,0>) -> FixedArray<Float64,0> { x }",0,int64_align,8),
                ("fn f(x: FixedArray<Unit,64>) -> FixedArray<Unit,64> { x }",0,1,0),
            ] {
                let ty = result(source);
                assert!(ty.scalar_types().is_empty(), "places have no scalar expansion");
                assert_eq!(ty.value_lanes().len(), 1);
                let layout=ty.memory_layout_for(target).unwrap();
                assert_eq!((layout.size,layout.align),(size,align));
                let MemoryLayoutShape::Array { stride: actual, .. }=layout.shape else { panic!() };
                assert_eq!(actual,stride);
            }
            let layout=result("struct C { tag: UInt8, data: FixedArray<Int32,3>, tail: UInt16 } fn f(x: C) -> C { x }").memory_layout_for(target).unwrap();
            assert_eq!((layout.size, layout.align), (20, 4));
            let MemoryLayoutShape::Record(fields) = layout.shape else {
                panic!()
            };
            assert_eq!(
                fields.iter().map(|f| f.offset).collect::<Vec<_>>(),
                [0, 4, 16]
            );
        }
    }
    #[test]
    fn fixed_array_f1_limits_and_elements_cannot_be_erased() {
        for ty in [
            "FixedArray<Unit,65537>",
            "FixedArray<FixedArray<Unit,65536>,2>",
            "FixedArray<FixedArray<Unit,65537>,0>",
            "FixedArray<(Unit,Unit),65536>",
            "Option<FixedArray<Unit,65536>>",
            "FixedArray<Ptr,0>",
            "FixedArray<String,0>",
            "FixedArray<fn()->Unit,0>",
            "FixedArray<&Int32,0>",
            "FixedArray<Phantom<Ptr>,0>",
        ] {
            let source=format!("import joky/option.{{type Option}}\nstruct Phantom<T> {{}} fn f(x: {ty}) -> {ty} {{ x }}");
            assert!(normalized(&source).is_err(), "{ty}");
        }
        for source in [
            "struct Recursive { child: FixedArray<Recursive,0> }",
            "type A = FixedArray<A,0>",
        ] {
            assert!(normalized(source).is_err(), "{source}");
        }
        assert!(
            normalized("fn f(x: FixedArray<Unit,65536>) -> FixedArray<Unit,65536> { x }").is_ok()
        );
    }
    #[test]
    fn fixed_array_f1_const_identity_and_substitution_are_rigid() {
        let parsed = crate::parser::parse(
            "fn f<const N: UInt64>() -> UInt64 { N } fn g<const N: UInt64>() -> UInt64 { N }",
        )
        .unwrap();
        let Item::FnDef(f) = &parsed.items[0] else {
            panic!()
        };
        let Item::FnDef(g) = &parsed.items[1] else {
            panic!()
        };
        assert_ne!(f.type_params[0].argument(), g.type_params[0].argument());
        let local = TypeRegistry::new().with_parameters(&f.type_params).unwrap();
        let ty = local
            .fixed_array(
                Type::Int32,
                ConstArgument::Parameter(f.type_params[0].clone()),
            )
            .unwrap();
        let substitutions = [(
            f.type_params[0].id,
            GenericArgument::Const(ConstArgument::Value(4)),
        )]
        .into_iter()
        .collect();
        let concrete = substitute(&ty, &substitutions, &local).unwrap();
        assert_eq!(concrete.memory_layout().unwrap().size, 16);
        let mut inferred = Substitutions::default();
        unify(
            &ty,
            &concrete,
            &f.type_params,
            &mut inferred,
            Span::empty(0),
        )
        .unwrap();
        assert_eq!(
            inferred.argument(&f.type_params[0]),
            Some(GenericArgument::Const(ConstArgument::Value(4)))
        );
        let mismatch = local
            .fixed_array(Type::Int32, ConstArgument::Value(5))
            .unwrap();
        assert!(unify(
            &ty,
            &mismatch,
            &f.type_params,
            &mut inferred,
            Span::empty(0)
        )
        .is_err());
    }
    #[test]
    fn fixed_array_f1_shape_byte_and_dag_budgets_are_bounded() {
        let registry = TypeRegistry::new();
        for n in 0..256 {
            registry
                .fixed_array(Type::Unit, ConstArgument::Value(n))
                .unwrap();
        }
        assert!(registry
            .fixed_array(Type::Unit, ConstArgument::Value(255))
            .is_ok());
        assert_eq!(
            registry
                .fixed_array(Type::Unit, ConstArgument::Value(256))
                .unwrap_err()
                .code(),
            ErrorCode::ResourceLimit
        );
        assert!(registry
            .compilation_scope()
            .fixed_array(Type::Unit, ConstArgument::Value(256))
            .is_ok());
        let ty = result("fn f(x:FixedArray<Int64,4>) -> FixedArray<Int64,4> { x }");
        assert!(ty
            .memory_layout_for(MemoryLayoutTarget {
                max_size: 31,
                ..Default::default()
            })
            .is_err());
        assert!(ty
            .memory_layout_for(MemoryLayoutTarget {
                int64_align: MAX_BYTES,
                ..Default::default()
            })
            .is_err());
        assert!(ty.memory_scalars().is_err());
        assert!(ty.storage_scalars().is_err());
        let mut source = String::from("type A0 = FixedArray<Unit,0>\n");
        for n in 1..=17 {
            source.push_str(&format!("type A{n} = (A{},A{})\n", n - 1, n - 1));
        }
        assert!(normalized(&source).is_err());
        assert!(normalized(
            "struct Phantom<T> {} type Bad = Phantom<FixedArray<FixedArray<Unit,65536>,2>>"
        )
        .is_err());
        let own=TypeRegistry::new().with_prelude(false).resolve_aliases(&crate::parser::parse("type FixedArray<T,const N: UInt64> = T fn f(x:FixedArray<Int32,4>) -> Int32 { x }").unwrap()).unwrap();
        let Item::FnDef(f) = &own.items[0] else {
            panic!()
        };
        assert_eq!(f.params[0].ty, Type::Int32);
        assert!(TypeRegistry::new()
            .resolve_aliases(&crate::parser::parse("fn f(x:FixedArray<Int32,4>) {}").unwrap())
            .is_err());
    }
}
