//! Semantic packet kinds are independent of flattened ABI lane counts.
use crate::{Error, Type};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Kind {
    Scalar = 0,
    String = 1,
    Tuple = 2,
    Record = 3,
    Enum = 4,
    Local = 5,
    Array = 6,
    FixedArray = 7,
    List = 8,
    Map = 9,
    Set = 10,
}

impl Kind {
    pub(crate) fn of(ty: &Type) -> Self {
        match ty {
            Type::FixedArray(_) => Self::FixedArray,
            Type::String => Self::String,
            Type::Record(r) if r.array => Self::Array,
            Type::Record(r) if r.list => Self::List,
            Type::Record(r) if r.map => Self::Map,
            Type::Record(r) if r.set => Self::Set,
            Type::Tuple(_) => Self::Tuple,
            Type::Record(r) if r.closure.is_none() => Self::Record,
            Type::Enum(_) => Self::Enum,
            _ if crate::core::instance::scalar(ty) => Self::Scalar,
            _ => Self::Local,
        }
    }

    pub(crate) fn decode(value: usize) -> Option<Self> {
        Some(match value {
            0 => Self::Scalar,
            1 => Self::String,
            2 => Self::Tuple,
            3 => Self::Record,
            4 => Self::Enum,
            5 => Self::Local,
            6 => Self::Array,
            7 => Self::FixedArray,
            8 => Self::List,
            9 => Self::Map,
            10 => Self::Set,
            _ => return None,
        })
    }

    /// Physical packet kinds implemented by every backend.
    pub(crate) fn implemented(self) -> bool {
        !matches!(self, Self::Local)
    }
}

/// Classify the whole nominal tree, not just currently active physical lanes.
/// This describes packet eligibility; it is not an executable transport certificate.
pub(crate) fn classify(
    root: &Type,
    resolve: &impl Fn(&Type) -> Result<Type, Error>,
    has_drop: &impl Fn(&Type) -> bool,
) -> Option<Kind> {
    let root = resolve(root).ok()?;
    let kind = Kind::of(&root);
    let mut pending = vec![(root, 0)];
    let mut visited = 0;
    while let Some((ty, depth)) = pending.pop() {
        visited += 1;
        if depth > 32 || visited > 65_536 {
            return None;
        }
        let ty = resolve(&ty).ok()?;
        if has_drop(&ty) || ty.storage_layout().is_err() {
            return None;
        }
        let children: Vec<Type> = match &ty {
            Type::FixedArray(a) if ty.is_fixed_array_data() => vec![a.element.clone()],
            Type::Tuple(t) => t.elements.clone(),
            Type::Record(r) if r.array => {
                let [field] = r.fields.as_slice() else {
                    return None;
                };
                let Type::Buffer(element) = &field.ty else {
                    return None;
                };
                vec![(**element).clone()]
            }
            Type::Record(r) if r.closure.is_none() && !r.indirect => r
                .arguments
                .iter()
                .filter_map(|a| a.as_type())
                .cloned()
                .chain(r.fields.iter().map(|f| f.ty.clone()))
                .collect(),
            Type::Enum(e) if !e.indirect && !e.variants.is_empty() => e
                .arguments
                .iter()
                .filter_map(|a| a.as_type())
                .cloned()
                .chain(e.variants.iter().filter_map(|v| v.payload.clone()))
                .collect(),
            Type::String => Vec::new(),
            _ if crate::core::instance::scalar(&ty) => Vec::new(),
            _ => return None,
        };
        pending.extend(children.into_iter().map(|ty| (ty, depth + 1)));
    }
    Some(kind)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::parallel::packet::Shape;
    use crate::runtime::task_scope::layout::{encode, CheckedLayout};

    #[test]
    fn task_packet_kinds_check_nominal_children_and_preserve_transport_gate() {
        let source = r#"
            import joky/drop.{type Drop}
            import joky/option.{type Option}
            import joky/result.{type Result}
            import joky/array.{type Array}
            import joky/map.{type Map}
            import joky/list.{type List}
            import joky/set.{type Set}
            import joky/fixed_array.{type FixedArray}
            struct One { value: Int32 }
            struct Empty { }
            struct Box<T> { value: T }
            struct Marker<T> { value: Int32 }
            struct Tracked { text: String }
            impl Drop for Tracked { fn drop(&mut self) { } }
            type Alias = Box<(Int8, String)>
            fn scalar(value: Int32) { }
            fn text(value: String) { }
            fn unit(value: Unit) { }
            fn one(value: One) { }
            fn empty(value: Empty) { }
            fn tuple(value: (Int8, UInt16)) { }
            fn record(value: Alias) { }
            fn optional(value: Option<Alias>) { }
            fn outcome(value: Result<Alias, String>) { }
            fn borrowed(value: Box<&Int32>) { }
            fn pointer(value: Option<Ptr>) { }
            fn phantom(value: Marker<Ptr>) { }
            fn tracked(value: Box<Tracked>) { }
            fn inactive_drop(value: Result<Int32, Tracked>) { }
            fn container(value: Box<Array<Int32>>) { }
            fn array_value(value: Array<Array<Alias>>) { }
            fn array_pointer(value: Array<Ptr>) { }
            fn array_reference(value: Array<&Int32>) { }
            fn array_phantom(value: Array<Marker<Ptr>>) { }
            fn array_drop(value: Array<Result<Int32, Tracked>>) { }
            fn array_map(value: Array<Map<Int32, String>>) { }
            fn array_list(value: Array<List<String>>) { }
            fn list_value(value: List<List<String>>) { }
            fn list_pointer(value: List<Ptr>) { }
            fn list_reference(value: List<&Int32>) { }
            fn list_phantom(value: List<Marker<Ptr>>) { }
            fn list_drop(value: List<Result<Int32, Tracked>>) { }
            fn list_map(value: List<Map<Int32, String>>) { }
            fn list_set(value: List<Set<Int32>>) { }
            fn array_set(value: Array<Set<Int32>>) { }
            fn array_fixed(value: Array<FixedArray<Int32, 2>>) { }
            fn map_value(value: Map<String, List<Array<String>>>) { }
            fn set_value(value: Set<String>) { }
            fn map_key_pointer(value: Map<Ptr, Int32>) { }
            fn map_value_reference(value: Map<Int32, &Int32>) { }
            fn map_phantom(value: Map<Marker<Ptr>, Int32>) { }
            fn map_drop(value: Map<Int32, Result<Int32, Tracked>>) { }
            fn set_pointer(value: Set<Ptr>) { }
            fn set_reference(value: Set<&Int32>) { }
            fn set_drop(value: Set<Tracked>) { }
            fn callable(value: fn(Int32) -> Int32) { }
            fn child() @suspends -> One { One { value: 42 } }
            fn join_value() @suspends -> Int32 {
                let (a, b) = join { | child() | child() }
                a.value + b.value
            }
            fn race_value() @suspends -> Int32 {
                let a = race { | child() | child() }
                a.value
            }
            fn branch_value(value: One) @suspends { scope { branch { one(value) } } }
        "#;
        let types = crate::TypeRegistry::default().with_prelude(false);
        let ast = types
            .resolve_with_modules(&crate::parser::parse(source).unwrap(), &Default::default())
            .unwrap();
        let prepared =
            crate::core::prepared::PreparedModule::new(&ast, &types, &Default::default()).unwrap();
        let plan = prepared.plan(&Default::default()).unwrap();
        let effects = super::super::analyse(&plan, &prepared, &Default::default());
        for name in ["join_value", "race_value", "branch_value"] {
            effects.require(name).unwrap();
        }
        for (name, expected) in [
            ("scalar", Some(Kind::Scalar)),
            ("text", Some(Kind::String)),
            ("unit", Some(Kind::Scalar)),
            ("one", Some(Kind::Record)),
            ("empty", Some(Kind::Record)),
            ("tuple", Some(Kind::Tuple)),
            ("record", Some(Kind::Record)),
            ("optional", Some(Kind::Enum)),
            ("outcome", Some(Kind::Enum)),
            ("borrowed", None),
            ("pointer", None),
            ("phantom", None),
            ("tracked", None),
            ("inactive_drop", None),
            ("container", Some(Kind::Record)),
            ("array_value", Some(Kind::Array)),
            ("array_pointer", None),
            ("array_reference", None),
            ("array_phantom", None),
            ("array_drop", None),
            ("array_map", Some(Kind::Array)),
            ("array_list", Some(Kind::Array)),
            ("array_set", Some(Kind::Array)),
            ("list_value", Some(Kind::List)),
            ("list_pointer", None),
            ("list_reference", None),
            ("list_phantom", None),
            ("list_drop", None),
            ("list_map", Some(Kind::List)),
            ("list_set", Some(Kind::List)),
            ("array_fixed", Some(Kind::Array)),
            ("map_value", Some(Kind::Map)),
            ("set_value", Some(Kind::Set)),
            ("map_key_pointer", None),
            ("map_value_reference", None),
            ("map_phantom", None),
            ("map_drop", None),
            ("set_pointer", None),
            ("set_reference", None),
            ("set_drop", None),
            ("callable", None),
        ] {
            let signature = &plan.functions[name].signature;
            let ty = &signature.params[0];
            let kind = classify(ty, &|ty| Ok(ty.clone()), &|ty| {
                plan.bodies[name].has_custom_drop(ty)
            });
            assert_eq!(kind, expected, "{name}");
            assert_eq!(
                effects.require_packet(name).is_ok(),
                expected.is_some(),
                "{name} root admission"
            );
            let words = encode(signature).unwrap();
            let layout = CheckedLayout::decode(&words).unwrap();
            assert_eq!(layout.slot_counts().0, ty.parameter_slots(), "{name}");
            assert_eq!(
                layout.parallel_shapes().is_ok(),
                Shape::of(ty).is_ok(),
                "{name} must not acquire transport from its lane shape"
            );
        }
        // Equal payload layouts must still retain distinct semantic kinds.
        // Unit alone retains its legacy parameter placeholder.
        for (scalar, aggregate, slots) in [("scalar", "one", (1, 1)), ("unit", "empty", (1, 0))] {
            let scalar = encode(&plan.functions[scalar].signature).unwrap();
            let aggregate = encode(&plan.functions[aggregate].signature).unwrap();
            assert_eq!((scalar[3], aggregate[3]), slots);
            assert_eq!(&scalar[4..8], &aggregate[4..8]);
            assert_ne!(scalar[2], aggregate[2]);
        }
    }
}
