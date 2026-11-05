//! Semantic packet kinds are independent of flattened ABI lane counts.
use crate::{core::types::GenericArgument, Error, Type};
use std::sync::Arc;

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
    Resource = 11,
    Function = 12,
    Dynamic = 13,
}

impl Kind {
    pub(crate) fn of(ty: &Type) -> Self {
        match ty {
            Type::FixedArray(_) => Self::FixedArray,
            Type::String => Self::String,
            Type::Function(_) => Self::Function,
            Type::DynTrait(_) => Self::Dynamic,
            Type::Named(n) if matches!(n.as_str(), "domain.Handle" | "admission.Token") => {
                Self::Scalar
            }
            Type::Resource(name)
                if (crate::runtime::tcp::transferable_type(name)
                    || crate::runtime::parallel::control::resource_type(name)
                    || crate::runtime::buffer::crypto::transferable_type(name)
                    || name.as_ref() == "postgres.Owner"
                    || crate::runtime::tls::transferable_type(name)
                    || crate::runtime::shutdown::transferable_type(name)) =>
            {
                Self::Resource
            }
            Type::Record(r) if r.array => Self::Array,
            Type::Record(r) if r.list => Self::List,
            Type::Record(r) if r.map => Self::Map,
            Type::Record(r) if r.set => Self::Set,
            Type::Tuple(_) => Self::Tuple,
            Type::Record(r) if r.closure.as_ref().is_none_or(|c| c.storable) => Self::Record,
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
            11 => Self::Resource,
            12 => Self::Function,
            13 => Self::Dynamic,
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
    checked_type(root, resolve, has_drop).map(|ty| Kind::of(&ty))
}

/// Freeze the same concrete layouts that admission checks. Buffer elements in
/// the language signature may still be shallow nominal identities.
pub(super) fn checked_type(
    root: &Type,
    resolve: &impl Fn(&Type) -> Result<Type, Error>,
    has_drop: &impl Fn(&Type) -> bool,
) -> Option<Type> {
    // Copy only the paths to resolved identities. Other fields (including
    // shared aggregate DAGs) keep the module's original layout nodes.
    fn visit(
        ty: &mut Type,
        resolve: &impl Fn(&Type) -> Result<Type, Error>,
        has_drop: &impl Fn(&Type) -> bool,
        depth: usize,
        remaining: &mut usize,
    ) -> Option<bool> {
        *remaining = remaining.checked_sub(1)?;
        if depth > 32 {
            return None;
        }
        let mut changed = matches!(ty, Type::Record(r) if r.indirect)
            || matches!(ty, Type::Enum(e) if e.indirect);
        if changed {
            *ty = resolve(ty).ok()?;
        }
        if matches!(ty, Type::Enum(ref e) if e.compact)
            || has_drop(ty)
            || ty.storage_layout().is_err()
        {
            return None;
        }
        let mut child = |source: &Type| {
            let mut layout = source.clone();
            let updated = visit(&mut layout, resolve, has_drop, depth + 1, remaining)?;
            changed |= updated;
            Some(updated.then_some(layout))
        };
        let kind = Kind::of(ty);
        match ty {
            Type::FixedArray(a) if a.element.is_fixed_array_data() => {
                if let Some(element) = child(&a.element)? {
                    Arc::make_mut(a).element = element;
                }
            }
            Type::Tuple(t) => {
                for i in 0..t.elements.len() {
                    if let Some(element) = child(&t.elements[i])? {
                        Arc::make_mut(t).elements[i] = element;
                    }
                }
            }
            Type::Record(r) if r.array => {
                let [field] = r.fields.as_slice() else {
                    return None;
                };
                let Type::Buffer(element) = &field.ty else {
                    return None;
                };
                if let Some(element) = child(element)? {
                    Arc::make_mut(r).fields[0].ty = Type::Buffer(Arc::new(element));
                }
            }
            Type::Record(r) if !r.indirect && r.closure.as_ref().is_none_or(|c| c.storable) => {
                // A closure's first field is the compiler-owned null cleanup identity.
                let start = usize::from(r.closure.is_some());
                if r.closure.is_none() {
                    for i in 0..r.arguments.len() {
                        if let GenericArgument::Type(t) = &r.arguments[i] {
                            if let Some(layout) = child(t)? {
                                Arc::make_mut(r).arguments[i] = GenericArgument::Type(layout);
                            }
                        }
                    }
                }
                for i in start..r.fields.len() {
                    if let Some(layout) = child(&r.fields[i].ty)? {
                        Arc::make_mut(r).fields[i].ty = layout;
                    }
                }
            }
            Type::Enum(e) if !e.indirect && !e.variants.is_empty() => {
                for i in 0..e.arguments.len() {
                    if let GenericArgument::Type(t) = &e.arguments[i] {
                        if let Some(layout) = child(t)? {
                            Arc::make_mut(e).arguments[i] = GenericArgument::Type(layout);
                        }
                    }
                }
                for i in 0..e.variants.len() {
                    if let Some(payload) = &e.variants[i].payload {
                        if let Some(layout) = child(payload)? {
                            Arc::make_mut(e).variants[i].payload = Some(layout);
                        }
                    }
                }
            }
            _ if matches!(
                kind,
                Kind::Scalar | Kind::String | Kind::Function | Kind::Resource | Kind::Dynamic
            ) => {}
            _ => return None,
        }
        Some(changed)
    }
    let mut ty = root.clone();
    visit(&mut ty, resolve, has_drop, 0, &mut 65_536)?;
    Some(ty)
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
                let (a, b) = join { child(), child() }
                a.value + b.value
            }
            fn race_value() @suspends -> Int32 {
                let a = race { child(), child() }
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
            ("callable", Some(Kind::Function)),
        ] {
            let signature = &plan.functions[name].signature;
            let ty = &signature.params[0];
            let packet = effects.require_packet(name);
            assert_eq!(packet.is_ok(), expected.is_some(), "{name} root admission");
            if let Ok(packet) = packet {
                // The host has only the frozen signature, without the module's
                // nominal table. Every admitted child must already be concrete.
                let kind = classify(&packet.params[0], &|ty| Ok(ty.clone()), &|ty| {
                    plan.bodies[name].has_custom_drop(ty)
                });
                assert_eq!(kind, expected, "{name} frozen packet");
            }
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
